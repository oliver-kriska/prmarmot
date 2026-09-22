//! Rendering a [`BoardView`] as JSON (for agents), Markdown (for agents and
//! documents), or a terminal table. Sections and order come from the shared
//! core layout; wording and tone mirror the app's table (`src/table.rs`),
//! which is where presentation rules live — core only owns the facts.

use chrono::Local;
use prmarmot_core::board::{
    strip_note_glyphs, Blocker, BoardRow, BoardScope, Category, Ci, Mode, QueueProvenance,
    ReviewState,
};
use prmarmot_core::layout::{section_explanation, LayoutItem, SectionKind, Sort};
use prmarmot_core::pickup::{wait_label, waiting_secs};
use prmarmot_core::status::{
    all_open_count, all_open_local_filter_notice, all_open_no_match_text, queue_empty_text,
};
use serde_json::{json, Value};

use crate::term::{display_width, fit, truncate, Paint, Tone};
use crate::view::{BoardView, Marks};

pub const BOARD_SCHEMA: &str = "prmarmot-cli/board@1";

/// The app's switcher label: across all repositories the authored search
/// widens to every open PR involving you, unless `--authored` keeps it to yours.
pub fn view_title(mode: Mode, scope: &BoardScope, authored_only: bool) -> &'static str {
    match mode {
        Mode::Authored if scope.is_all() && !authored_only => "Involving me",
        Mode::Authored => "My PRs",
        Mode::Review => "Review queue",
        Mode::AllOpen => "All open",
    }
}

pub fn mode_key(mode: Mode) -> &'static str {
    match mode {
        Mode::Authored => "authored",
        Mode::Review => "review",
        Mode::AllOpen => "all",
    }
}

pub fn scope_label(scope: &BoardScope) -> String {
    match scope {
        BoardScope::AllRepositories => "all repositories".into(),
        BoardScope::Repository(repo) => repo.clone(),
    }
}

pub fn sort_key(sort: Sort) -> &'static str {
    match sort {
        Sort::Wait => "wait",
        Sort::Smallest => "smallest",
    }
}

pub fn scope_json(scope: &BoardScope) -> Value {
    match scope {
        BoardScope::AllRepositories => json!({ "type": "all" }),
        BoardScope::Repository(repo) => json!({ "type": "repository", "repo": repo }),
    }
}

fn empty_message(view: &BoardView) -> String {
    // All open: when GitHub answered the whole filter, or every open PR is
    // loaded, nothing matching is a fact about the repository.
    let query = view.filters.query.as_deref().map(str::trim);
    if let Some(query) = query.filter(|query| !query.is_empty()) {
        let flags = view.filters.changed || view.filters.watched || view.filters.stale;
        if view.mode == Mode::AllOpen
            && !flags
            && (!view.truncated || view.local_only_terms().is_empty())
        {
            return all_open_no_match_text(query);
        }
    }
    if view.filtered_out > 0 {
        return "No PRs match the filter".into();
    }
    match view.mode {
        Mode::Authored if view.all_repos() && !view.authored_only => {
            "No open pull requests involve you".into()
        }
        Mode::Authored => "You have no open PRs".into(),
        Mode::Review => "No requested or available reviews in this result set".into(),
        Mode::AllOpen => queue_empty_text(view.mode, view.all_repos()).into(),
    }
}

/// All open and the review queue name each PR's author in their own column;
/// All open's Notes never do.
fn author_column(mode: Mode) -> bool {
    matches!(mode, Mode::Review | Mode::AllOpen)
}

// ---- JSON ------------------------------------------------------------------

fn ci_key(ci: Ci) -> &'static str {
    ci.as_str()
}

fn blocker_json(blocker: &Blocker) -> Value {
    match blocker {
        Blocker::NoReviewers { suggested } => {
            json!({ "type": "no_reviewers", "suggested": suggested })
        }
        Blocker::MergeConflict => json!({ "type": "merge_conflict" }),
        Blocker::CiFailing => json!({ "type": "ci_failing" }),
        Blocker::ChangesRequested => json!({ "type": "changes_requested" }),
        Blocker::UnresolvedComments(count) => {
            json!({ "type": "unresolved_comments", "count": count })
        }
    }
}

/// One PR with every fact the app shows, plus the attention overlay.
pub fn pr_json(row: &BoardRow, marks: &Marks) -> Value {
    json!({
        "id": row.id,
        "repo": row.repo,
        "number": row.number,
        "url": row.url,
        "title": row.title,
        "author": row.author,
        "draft": row.draft,
        "category": row.category.as_str(),
        "queue": row.queue_provenance.map(|queue| match queue {
            QueueProvenance::Requested => "requested",
            QueueProvenance::Available => "available",
        }),
        "ci": ci_key(row.ci),
        "conflict": row.conflict,
        "review_decision": row.review_decision,
        "review_state": row.review_state.as_str(),
        "requested_reviewers": row.requested,
        "reviews": row.reviews.iter().map(|review| json!({
            "login": review.login,
            "state": review.state,
            "submitted_at": review.submitted_at,
        })).collect::<Vec<_>>(),
        "my_review": row.my_review,
        "unresolved_threads": row.unresolved,
        "labels": row.labels,
        "issue": row.issue.as_ref().map(|key| json!({ "key": key, "url": row.issue_url })),
        "stack": row.stack.as_ref().map(|stack| json!({
            "number": stack.number,
            "size": stack.size,
            "position": stack.position,
            "base_ref": stack.base_ref_name,
        })),
        "blockers": row.blockers.iter().map(blocker_json).collect::<Vec<_>>(),
        "note": strip_note_glyphs(&row.note),
        "head_oid": row.head_oid,
        "created_at": row.created_at,
        "updated_at": row.updated_at,
        "waiting_since": row.waiting_since,
        "stale": marks.stale,
        "size": row.size.map(|size| json!({
            "band": size.band().key(),
            "additions": size.additions,
            "deletions": size.deletions,
            "changed_files": size.changed_files,
        })),
        "attention": {
            "watched": marks.watched,
            "snoozed": marks.snoozed.as_ref().map(|description| json!({ "description": description })),
            "changed": marks.changed,
            "changes": marks.changes,
        },
    })
}

pub fn rate_json(view: &BoardView) -> Value {
    view.rate
        .as_ref()
        .map(|rate| {
            json!({
                "limit": rate.limit,
                "remaining": rate.remaining,
                "cost": rate.cost,
                "reset_at": rate.reset_at,
            })
        })
        .unwrap_or(Value::Null)
}

/// The complete board: every section including Snoozed, each PR in display
/// order. Stable keys; additive changes only within `board@1`.
pub fn board_json(view: &BoardView) -> Value {
    let mut sections = Vec::new();
    for item in view.layout(false) {
        if let LayoutItem::Header {
            kind,
            label,
            count: Some(count),
            members,
            ..
        } = item
        {
            sections.push(json!({
                "key": kind.key(),
                "label": label,
                "explanation": section_explanation(view.mode, kind, view.all_repos()),
                "count": count,
                "prs": members
                    .iter()
                    .map(|&ix| pr_json(&view.rows[ix], &view.marks[ix]))
                    .collect::<Vec<_>>(),
            }));
        }
    }
    json!({
        "schema": BOARD_SCHEMA,
        "generated_at": view.generated_at.to_rfc3339(),
        "viewer": view.viewer,
        "mode": mode_key(view.mode),
        "view": view_title(view.mode, &view.scope, view.authored_only),
        "scope": scope_json(&view.scope),
        "sort": sort_key(view.sort),
        "count": view.rows.len(),
        "total": view.total,
        "filters": {
            "changed": view.filters.changed,
            "watched": view.filters.watched,
            "stale": view.filters.stale,
            "stale_after_days": view.filters.stale_after_days,
            "filtered_out": view.filtered_out,
        },
        "truncated": view.truncated,
        "more_pages_available": view.can_load_more,
        "rate_limit": rate_json(view),
        "attention_state_error": view.attention_error,
        "sections": sections,
    })
}

// ---- Shared cell text --------------------------------------------------------

fn pr_ref(row: &BoardRow, all_repos: bool) -> String {
    if all_repos {
        format!("{}#{}", row.repo, row.number)
    } else {
        format!("#{}", row.number)
    }
}

fn ci_cell(ci: Ci) -> (&'static str, Tone) {
    match ci {
        Ci::Pass => ("pass", Tone::Success),
        Ci::Fail => ("fail", Tone::Danger),
        Ci::Running => ("running", Tone::Warning),
        Ci::None => ("none", Tone::Muted),
        Ci::Hidden => ("hidden", Tone::Muted),
    }
}

fn review_glyph(state: &str) -> (&'static str, Tone) {
    match state {
        "APPROVED" => ("✓", Tone::Success),
        "CHANGES_REQUESTED" => ("±", Tone::Danger),
        "DISMISSED" => ("✕", Tone::Muted),
        _ => ("·", Tone::Muted),
    }
}

fn review_aggregate(state: ReviewState) -> &'static str {
    match state {
        ReviewState::Approved => "approved",
        ReviewState::Changes => "changes requested",
        ReviewState::Commented => "commented",
        ReviewState::Waiting => "requested",
        ReviewState::None => "reviewed",
    }
}

/// Completed reviews win over pending requests, as in the app's Review column.
fn review_text(row: &BoardRow) -> String {
    if !row.reviews.is_empty() {
        let reviewers = row
            .reviews
            .iter()
            .map(|review| {
                format!(
                    "{} {}",
                    review_glyph(&review.state).0,
                    review.login.as_deref().unwrap_or("?")
                )
            })
            .collect::<Vec<_>>()
            .join(" ");
        format!("{reviewers} — {}", review_aggregate(row.review_state))
    } else if !row.requested.is_empty() {
        format!("→ {} — requested", row.requested.join(", "))
    } else {
        "not requested".into()
    }
}

fn title_text(row: &BoardRow, stack_branch: Option<&str>) -> String {
    let mut out = String::new();
    if let (Some(stack), Some(branch)) = (&row.stack, stack_branch) {
        let position = stack
            .position
            .map(|p| p.to_string())
            .unwrap_or_else(|| "?".into());
        out.push_str(&format!("{branch} {position}/{} ", stack.size));
    }
    if let Some(issue) = &row.issue {
        out.push_str(&format!("{issue} · "));
    }
    out.push_str(&row.title);
    out
}

/// The Note as primary phrase, optional remedy and context facts, with the
/// app's tone: exceptional blockers are danger, routine follow-up a warning.
pub struct Note {
    pub tone: Tone,
    pub text: String,
}

fn blocker_rank(blocker: &Blocker) -> u8 {
    match blocker {
        Blocker::MergeConflict => 0,
        Blocker::CiFailing => 1,
        Blocker::ChangesRequested => 2,
        Blocker::UnresolvedComments(_) => 3,
        Blocker::NoReviewers { .. } => 4,
    }
}

fn blocker_primary(blocker: &Blocker) -> String {
    match blocker {
        Blocker::MergeConflict => "merge conflict — rebase".into(),
        Blocker::CiFailing => "CI failing".into(),
        Blocker::ChangesRequested => "changes requested".into(),
        Blocker::UnresolvedComments(n) => {
            format!("resolve {n} comment{}", if *n == 1 { "" } else { "s" })
        }
        Blocker::NoReviewers { suggested } if suggested.is_empty() => "assign reviewers".into(),
        Blocker::NoReviewers { suggested } => format!("assign {}", suggested.join(" + ")),
    }
}

fn blocker_context(blocker: &Blocker) -> String {
    match blocker {
        Blocker::MergeConflict => "merge conflict".into(),
        Blocker::CiFailing => "CI failing".into(),
        Blocker::ChangesRequested => "changes requested".into(),
        Blocker::UnresolvedComments(n) => format!("{n} unresolved"),
        Blocker::NoReviewers { .. } => "reviewers missing".into(),
    }
}

pub fn note(row: &BoardRow) -> Note {
    let plain = strip_note_glyphs(&row.note);
    match row.category {
        Category::Action => {
            let mut ranked: Vec<&Blocker> = row.blockers.iter().collect();
            ranked.sort_by_key(|b| blocker_rank(b));
            let Some((primary, rest)) = ranked.split_first() else {
                return Note {
                    tone: Tone::Warning,
                    text: plain,
                };
            };
            let tone = if matches!(
                primary,
                Blocker::MergeConflict | Blocker::CiFailing | Blocker::ChangesRequested
            ) {
                Tone::Danger
            } else {
                Tone::Warning
            };
            let mut text = blocker_primary(primary);
            for blocker in rest {
                text.push_str(" · ");
                text.push_str(&blocker_context(blocker));
            }
            Note { tone, text }
        }
        Category::Todo | Category::Available if row.ci == Ci::Fail || row.conflict => Note {
            tone: Tone::Danger,
            text: plain,
        },
        Category::Todo | Category::Available => Note {
            tone: Tone::Warning,
            text: plain,
        },
        Category::Await => Note {
            tone: Tone::Success,
            text: plain,
        },
        Category::Done if row.my_review.as_deref() == Some("APPROVED") => Note {
            tone: Tone::Success,
            text: plain,
        },
        Category::Done | Category::Draft => Note {
            tone: Tone::Muted,
            text: plain,
        },
    }
}

/// The Note plus how long the PR has waited for a reviewer (a stale wait
/// warns) and, in the review queue and All open, the size band.
fn note_for(view: &BoardView, ix: usize) -> Note {
    let row = &view.rows[ix];
    let mut note = note(row);
    if let Some(secs) = waiting_secs(row, view.generated_at) {
        note.text
            .push_str(&format!(" · waiting {}", wait_label(secs)));
        if view.marks[ix].stale {
            note.text.push_str(" (stale)");
            if note.tone != Tone::Danger {
                note.tone = Tone::Warning;
            }
        }
    }
    if let Some(size) = row.size.filter(|_| author_column(view.mode)) {
        note.text.push_str(&format!(" · {}", size.band().label()));
    }
    note
}

/// All open counts against what GitHub found: "60 of 761 open", or "match"
/// when a `label:` or `author:` filter went with the search, as the app's
/// header says it. `None` for the other views, and when GitHub did not say.
fn found_count(view: &BoardView) -> Option<String> {
    let total = view.total.filter(|_| view.mode == Mode::AllOpen)?;
    Some(all_open_count(
        view.loaded(),
        total,
        !view.remote.is_empty(),
    ))
}

fn status_line(view: &BoardView) -> String {
    let mut parts = vec![scope_label(&view.scope)];
    match found_count(view) {
        // Some loaded PRs didn't pass the filters: how many are listed.
        Some(found) if view.filtered_out > 0 => {
            parts.push(found);
            parts.push(format!("{} shown", view.rows.len()));
        }
        Some(found) => parts.push(found),
        None => parts.push(format!(
            "{} PR{}",
            view.rows.len(),
            if view.rows.len() == 1 { "" } else { "s" }
        )),
    }
    parts.push(format!(
        "synced {}",
        view.generated_at.with_timezone(&Local).format("%H:%M")
    ));
    if view.sort == Sort::Smallest {
        parts.push("smallest first".into());
    }
    if let Some(rate) = &view.rate {
        parts.push(format!("API {}/{}", rate.remaining, rate.limit));
    }
    parts.join(" · ")
}

fn filter_line(view: &BoardView) -> Option<String> {
    let mut active = Vec::new();
    if view.filters.changed {
        active.push("changed".to_owned());
    }
    if view.filters.watched {
        active.push("watched".to_owned());
    }
    if view.filters.stale {
        active.push(format!(
            "stale ({}d+ waiting)",
            view.filters.stale_after_days
        ));
    }
    (!active.is_empty()).then(|| {
        format!(
            "Filter: {} · {} hidden",
            active.join(" + "),
            view.filtered_out
        )
    })
}

fn footer_lines(view: &BoardView) -> Vec<String> {
    let mut lines = Vec::new();
    if view.can_load_more {
        lines.push("More results on GitHub — pass --pages N (up to 5) to load them".into());
    } else if view.truncated {
        lines.push("Results truncated at GitHub's page limit".into());
    }
    // The footer already says how to load more, so the notice doesn't.
    if view.mode == Mode::AllOpen {
        lines.extend(all_open_local_filter_notice(
            &view.local_only_terms(),
            view.loaded(),
            view.total,
            false,
        ));
    }
    if let Some(error) = &view.attention_error {
        lines.push(format!("Watch/snooze state unavailable: {error}"));
    }
    lines
}

/// Stack branch glyph for a row at `pos` in `items`: the tree ends where the
/// next row is not a layer of the same stack.
fn stack_branch(view: &BoardView, items: &[LayoutItem], pos: usize) -> Option<&'static str> {
    let LayoutItem::Row(ix) = items[pos] else {
        return None;
    };
    let row = &view.rows[ix];
    let stack = row.stack.as_ref()?;
    let continues = match items.get(pos + 1) {
        Some(LayoutItem::Row(next)) => {
            let next = &view.rows[*next];
            next.repo == row.repo
                && next
                    .stack
                    .as_ref()
                    .is_some_and(|s| s.number == stack.number)
        }
        _ => false,
    };
    Some(if continues { "├─" } else { "└─" })
}

// ---- Markdown ----------------------------------------------------------------

fn md_cell(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('|', "\\|")
        .replace(['\n', '\r'], " ")
}

fn md_attention(marks: &Marks) -> String {
    let mut parts = Vec::new();
    if marks.changed {
        let changes = if marks.changes.is_empty() {
            String::new()
        } else {
            format!(": {}", marks.changes.join("; "))
        };
        parts.push(format!("**changed**{changes}"));
    }
    if marks.watched {
        parts.push("watched".into());
    }
    if let Some(snooze) = &marks.snoozed {
        parts.push(snooze.to_lowercase());
    }
    parts.join(" · ")
}

pub fn markdown(view: &BoardView, show_snoozed: bool) -> String {
    let mut out = format!(
        "# {} · {}\n\n{}\n",
        view_title(view.mode, &view.scope, view.authored_only),
        scope_label(&view.scope),
        status_line(view)
    );
    if let Some(filter) = filter_line(view) {
        out.push_str(&format!("\n{filter}\n"));
    }
    let items = view.layout(show_snoozed);
    if view.rows.is_empty() {
        out.push_str(&format!("\n_{}_\n", empty_message(view)));
    }
    let author_mode = author_column(view.mode);
    let mut table_open = false;
    for (pos, item) in items.iter().enumerate() {
        match item {
            LayoutItem::Header {
                kind: SectionKind::Snoozed,
                count: Some(count),
                ..
            } if !show_snoozed => {
                out.push_str(&format!(
                    "\n_{count} snoozed PR{} hidden — pass `--snoozed` to include {}._\n",
                    if *count == 1 { "" } else { "s" },
                    if *count == 1 { "it" } else { "them" }
                ));
                table_open = false;
            }
            LayoutItem::Header {
                label,
                count: Some(count),
                ..
            } => {
                out.push_str(&format!("\n## {label} ({count})\n\n"));
                out.push_str(if author_mode {
                    "| PR | Title | Author | CI | Note |\n| --- | --- | --- | --- | --- |\n"
                } else {
                    "| PR | Title | CI | Review | Note |\n| --- | --- | --- | --- | --- |\n"
                });
                table_open = true;
            }
            // Stack sub-headers are implied by each layer's "├─ 1/3" prefix.
            LayoutItem::Header { .. } => {}
            LayoutItem::Row(ix) => {
                if !table_open {
                    continue;
                }
                let row = &view.rows[*ix];
                let marks = &view.marks[*ix];
                let mut note_text = note_for(view, *ix).text;
                let attention = md_attention(marks);
                if !attention.is_empty() {
                    note_text = format!("{note_text} · {attention}");
                }
                let title = title_text(row, stack_branch(view, &items, pos));
                let middle = if author_mode {
                    format!(
                        "{} | {}",
                        md_cell(row.author.as_deref().unwrap_or("?")),
                        ci_cell(row.ci).0
                    )
                } else {
                    format!("{} | {}", ci_cell(row.ci).0, md_cell(&review_text(row)))
                };
                out.push_str(&format!(
                    "| [{}]({}) | {} | {} | {} |\n",
                    md_cell(&pr_ref(row, view.all_repos())),
                    row.url,
                    md_cell(&title),
                    middle,
                    md_cell(&note_text)
                ));
            }
        }
    }
    let footer = footer_lines(view);
    if !footer.is_empty() {
        out.push('\n');
        for line in footer {
            out.push_str(&format!("_{line}_\n"));
        }
    }
    out
}

// ---- Terminal table ------------------------------------------------------------

const GAP: &str = "  ";
const CI_WIDTH: usize = 9;

struct Widths {
    pr: usize,
    title: usize,
    middle: usize,
    note: usize,
}

/// Size columns to their content so a wide terminal shows whole Notes. On a
/// narrow one the Note wins width over Title (DESIGN.md): Review/Author gives
/// way first, then Title down to about a third, and the Note last.
fn widths(view: &BoardView, items: &[LayoutItem], total: usize) -> Widths {
    let author_mode = author_column(view.mode);
    let (mut pr, mut title_need, mut middle_need, mut note_need) = (4, 12, 8, 12);
    for (pos, item) in items.iter().enumerate() {
        let LayoutItem::Row(ix) = item else {
            continue;
        };
        let row = &view.rows[*ix];
        pr = pr.max(display_width(&pr_ref(row, view.all_repos())));
        title_need = title_need.max(display_width(&title_text(
            row,
            stack_branch(view, items, pos),
        )));
        middle_need = middle_need.max(if author_mode {
            display_width(row.author.as_deref().unwrap_or("?"))
        } else {
            display_width(&review_text(row))
        });
        note_need = note_need.max(display_width(&note_for(view, *ix).text) + 2);
    }
    let pr = pr.min(40).min((total / 4).max(8));
    // marker(2) + space + pr + gap + title + gap + ci + gap + middle + gap + note
    let fixed = 2 + 1 + pr + GAP.len() * 4 + CI_WIDTH;
    let flex = total.saturating_sub(fixed).max(36);
    let mut note = note_need.min(72);
    let mut middle = middle_need.min(32);
    let mut title = title_need;
    let mut over = (note + middle + title).saturating_sub(flex);
    let mut give = |cell: &mut usize, floor: usize| {
        let cut = over.min(cell.saturating_sub(floor));
        *cell -= cut;
        over -= cut;
    };
    give(&mut middle, 12);
    give(&mut title, flex * 35 / 100);
    give(&mut note, 14);
    give(&mut title, 12);
    give(&mut middle, 8);
    Widths {
        pr,
        title,
        middle,
        note,
    }
}

/// A PR reference cut to `width` without losing its number: the repository
/// name gives way ("acme/very-long-na…#12").
fn fit_pr_ref(row: &BoardRow, all_repos: bool, width: usize) -> String {
    let full = pr_ref(row, all_repos);
    let number = format!("#{}", row.number);
    let room = width.saturating_sub(display_width(&number));
    if display_width(&full) <= width || !all_repos || room < 2 {
        return fit(&full, width);
    }
    fit(&format!("{}{number}", truncate(&row.repo, room)), width)
}

fn paint_leading(paint: &Paint, tone: Tone, cell: &str, lead: &str) -> String {
    match cell.strip_prefix(lead) {
        Some(rest) => format!("{}{rest}", paint.tone(tone, lead)),
        None => cell.to_owned(),
    }
}

pub fn table(view: &BoardView, width: usize, paint: Paint, show_snoozed: bool) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "{} {}\n",
        paint.bold(view_title(view.mode, &view.scope, view.authored_only)),
        paint.tone(Tone::Muted, &format!("· {}", status_line(view)))
    ));
    if let Some(filter) = filter_line(view) {
        out.push_str(&format!("{}\n", paint.tone(Tone::Muted, &filter)));
    }
    if view.rows.is_empty() {
        out.push_str(&format!(
            "\n{}\n",
            paint.tone(Tone::Muted, &empty_message(view))
        ));
    }
    let author_mode = author_column(view.mode);
    let items = view.layout(show_snoozed);
    let w = widths(view, &items, width);
    for (pos, item) in items.iter().enumerate() {
        match item {
            LayoutItem::Header {
                kind: SectionKind::Snoozed,
                count: Some(count),
                ..
            } if !show_snoozed => {
                out.push_str(&format!(
                    "\n{} {}\n",
                    paint.bold("Snoozed"),
                    paint.tone(
                        Tone::Muted,
                        &format!("{count} · hidden, pass --snoozed to show")
                    )
                ));
            }
            LayoutItem::Header {
                label,
                count: Some(count),
                ..
            } => {
                out.push_str(&format!(
                    "\n{} {}\n",
                    paint.bold(label),
                    paint.tone(Tone::Muted, &count.to_string())
                ));
            }
            LayoutItem::Header { label, detail, .. } => {
                let detail = detail
                    .as_deref()
                    .map(|d| format!(" · {d}"))
                    .unwrap_or_default();
                out.push_str(&format!(
                    "   {}\n",
                    paint.tone(Tone::Muted, &truncate(&format!("{label}{detail}"), width))
                ));
            }
            LayoutItem::Row(ix) => {
                let row = &view.rows[*ix];
                let marks = &view.marks[*ix];
                let marker = match (marks.changed, marks.watched) {
                    (true, true) => format!("{}◎", paint.tone(Tone::Accent, "●")),
                    (true, false) => format!("{} ", paint.tone(Tone::Accent, "●")),
                    (false, true) => format!("{} ", paint.tone(Tone::Muted, "◎")),
                    (false, false) => "  ".into(),
                };
                let pr = fit_pr_ref(row, view.all_repos(), w.pr);
                let title = fit(&title_text(row, stack_branch(view, &items, pos)), w.title);
                let (ci_word, ci_tone) = ci_cell(row.ci);
                let ci = format!(
                    "{} {}",
                    paint.tone(ci_tone, "●"),
                    fit(ci_word, CI_WIDTH - 2)
                );
                let middle = if author_mode {
                    fit(row.author.as_deref().unwrap_or("?"), w.middle)
                } else {
                    let text = fit(&review_text(row), w.middle);
                    match row.reviews.first() {
                        Some(first) => {
                            let (glyph, tone) = review_glyph(&first.state);
                            paint_leading(&paint, tone, &text, glyph)
                        }
                        None => paint.tone(Tone::Muted, &text),
                    }
                };
                let note = note_for(view, *ix);
                let note_text = truncate(&format!("● {}", note.text), w.note);
                let note_cell = if note.tone == Tone::Danger {
                    paint.tone(Tone::Danger, &note_text)
                } else {
                    paint_leading(&paint, note.tone, &note_text, "●")
                };
                out.push_str(&format!(
                    "{marker} {}{GAP}{title}{GAP}{ci}{GAP}{middle}{GAP}{note_cell}\n",
                    paint.tone(Tone::Accent, &pr),
                ));
                // What changed costs a line per PR; only when asked for.
                if view.filters.changed && !marks.changes.is_empty() {
                    let indent = 2 + 1 + w.pr + GAP.len();
                    out.push_str(&format!(
                        "{}{}\n",
                        " ".repeat(indent),
                        paint.tone(
                            Tone::Accent,
                            &truncate(
                                &format!("changed: {}", marks.changes.join("; ")),
                                width.saturating_sub(indent)
                            )
                        )
                    ));
                }
            }
        }
    }
    let mut footer = footer_lines(view);
    let any_changed = view.marks.iter().any(|m| m.changed);
    let any_watched = view.marks.iter().any(|m| m.watched);
    if any_changed || any_watched {
        let mut legend = Vec::new();
        if any_changed && !view.filters.changed {
            legend.push("● changed since you last looked (--changed lists what)");
        } else if any_changed {
            legend.push("● changed since you last looked");
        }
        if any_watched {
            legend.push("◎ watched");
        }
        footer.insert(0, legend.join(" · "));
    }
    if !footer.is_empty() {
        out.push('\n');
        for line in footer {
            out.push_str(&format!("{}\n", paint.tone(Tone::Muted, &line)));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::tests::{fetch_of, row};
    use crate::view::{build, Filters};
    use chrono::{TimeZone, Utc};
    use prmarmot_core::attention::SnapshotNamespace;
    use prmarmot_core::board::{Category, StackInfo};
    use prmarmot_core::size::ChangeSize;
    use prmarmot_local::attention_state::{AttentionState, SnoozeCondition};

    fn sample_view(mode: Mode) -> BoardView {
        let mut attention = AttentionState::empty(SnapshotNamespace::new("github.com", "me"));
        let mut action = row(10, Category::Action);
        action.ci = Ci::Fail;
        action.blockers = vec![Blocker::UnresolvedComments(2), Blocker::CiFailing];
        action.note = "🔴 CI failing · 2 unresolved".into();
        action.title = "Fix | pipes in titles".into();
        action.size = Some(ChangeSize {
            additions: 30,
            deletions: 12,
            changed_files: 3,
        });
        let mut approved = row(11, Category::Await);
        approved.review_state = ReviewState::Approved;
        approved.reviews[0].state = "APPROVED".into();
        let mut layer1 = row(12, Category::Action);
        layer1.blockers = vec![Blocker::NoReviewers {
            suggested: Vec::new(),
        }];
        layer1.stack = Some(StackInfo {
            number: 99,
            size: 2,
            base_ref_name: "main".into(),
            position: Some(1),
        });
        layer1.waiting_since = Some("2026-09-11T06:41:00Z".into());
        let mut layer2 = layer1.clone();
        layer2.id = "PR_13".into();
        layer2.number = 13;
        layer2.title = "Change number 13".into();
        layer2.url = "https://github.com/acme/widgets/pull/13".into();
        layer2.stack.as_mut().unwrap().position = Some(2);
        let snoozed = row(14, Category::Await);
        attention.toggle_watch(&action);
        attention.set_snooze(AttentionState::snooze_for(
            &snoozed,
            SnoozeCondition::Until {
                deadline: Utc::now() + chrono::Duration::hours(1),
            },
        ));
        build(
            fetch_of(vec![action, approved, layer2, layer1, snoozed]),
            &attention,
            mode,
            BoardScope::Repository("acme/widgets".into()),
            "me".into(),
            Filters::default(),
            Utc.with_ymd_and_hms(2026, 9, 15, 6, 41, 0).unwrap(),
        )
    }

    /// All open for acme/widgets: a review asked of you (by bob), someone
    /// else's ready PR (carol's, with a size), and a draft; GitHub found
    /// `total` open PRs and more pages exist.
    fn all_open_view(query: Option<&str>, total: Option<u64>) -> BoardView {
        let mut requested = row(21, Category::Todo);
        requested.author = Some("bob".into());
        requested.title = "Fix login redirect".into();
        requested.note = "🔵 needs your review".into();
        requested.labels = vec!["bug".into()];
        let mut open = row(22, Category::Await);
        open.author = Some("carol".into());
        open.note = "🟡 waiting on dave".into();
        open.size = Some(ChangeSize {
            additions: 40,
            deletions: 2,
            changed_files: 2,
        });
        let mut draft = row(23, Category::Draft);
        draft.author = Some("erin".into());
        draft.note = "draft".into();
        let mut fetch = fetch_of(vec![requested, open, draft]);
        fetch.total = total;
        fetch.truncated = total.is_some_and(|total| total > 3);
        build(
            fetch,
            &AttentionState::empty(SnapshotNamespace::new("github.com", "me")),
            Mode::AllOpen,
            BoardScope::Repository("acme/widgets".into()),
            "me".into(),
            Filters {
                query: query.map(str::to_owned),
                ..Filters::default()
            },
            Utc.with_ymd_and_hms(2026, 9, 21, 9, 0, 0).unwrap(),
        )
    }

    #[test]
    fn all_open_names_each_author_and_counts_against_what_github_found() {
        let view = all_open_view(None, Some(761));
        let md = markdown(&view, false);
        assert!(md.starts_with("# All open · acme/widgets\n\n"), "{md}");
        assert!(
            md.contains("acme/widgets · 3 of 761 open · synced "),
            "{md}"
        );
        assert!(md.contains("## Requested from you (1)"), "{md}");
        assert!(md.contains("## Awaiting review (1)"), "{md}");
        assert!(md.contains("## Drafts (1)"), "{md}");
        assert!(md.contains("| PR | Title | Author | CI | Note |"), "{md}");
        assert!(!md.contains("| Review |"), "{md}");
        // The Note never names the author; the column does. The size band
        // shows as in the review queue.
        assert!(
            md.contains("| Fix login redirect | bob | pass | needs your review"),
            "{md}"
        );
        assert!(
            md.contains("| carol | pass | waiting on dave · Small |"),
            "{md}"
        );
        // Nothing was filtered, so no notice.
        assert!(!md.contains("Only the"), "{md}");

        let text = table(&view, 120, Paint::new(false), false);
        assert!(
            text.starts_with("All open · acme/widgets · 3 of 761 open · synced "),
            "{text}"
        );
        assert!(text.lines().any(|line| line.contains("#22")
            && line.contains("carol")
            && line.contains("waiting on dave · Small")));

        let value = board_json(&view);
        assert_eq!(value["mode"], "all");
        assert_eq!(value["view"], "All open");
        assert_eq!(value["total"], 761);
        let keys: Vec<&str> = value["sections"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["key"].as_str().unwrap())
            .collect();
        assert_eq!(keys, ["todo", "await", "draft"]);
        // Each section says what puts a PR there, as the app's hover does.
        assert!(value["sections"][1]["explanation"]
            .as_str()
            .unwrap()
            .starts_with("Reviewers are asked or have commented"));
        assert_eq!(value["sections"][1]["prs"][0]["author"], "carol");

        // Everything loaded: the count is just what is open.
        assert!(status_line(&all_open_view(None, Some(3))).contains(" · 3 open · "));
        // GitHub didn't say: count the rows, as the other views do.
        assert!(status_line(&all_open_view(None, None)).contains(" · 3 PRs · "));
        assert_eq!(board_json(&all_open_view(None, None))["total"], Value::Null);
    }

    #[test]
    fn all_open_says_when_part_of_the_filter_only_checked_the_loaded_prs() {
        let view = all_open_view(Some("login is:stale"), Some(761));
        assert_eq!(view.rows.len(), 0);
        assert_eq!(view.filtered_out, 3);
        let notice = "Only the 3 loaded PRs of 761 are checked for is:stale and “login”.";
        let md = markdown(&view, false);
        assert!(md.contains(&format!("_{notice}_")), "{md}");
        assert!(md.contains("3 of 761 open · 0 shown"), "{md}");
        // Not the repository's answer: the notice says why.
        assert!(md.contains("_No PRs match the filter_"), "{md}");
        let text = table(&view, 120, Paint::new(false), false);
        assert!(text.contains(notice), "{text}");

        // A word that matches: the notice still says what wasn't checked.
        let view = all_open_view(Some("login"), Some(761));
        assert_eq!(view.rows.len(), 1);
        assert!(status_line(&view).contains("3 of 761 open · 1 shown"));
        assert_eq!(
            footer_lines(&view).last().map(String::as_str),
            Some("Only the 3 loaded PRs of 761 are checked for “login”.")
        );

        // `--stale` is `is:stale`.
        let mut view = all_open_view(None, Some(761));
        view.filters.stale = true;
        assert!(footer_lines(&view)
            .iter()
            .any(|line| line == "Only the 3 loaded PRs of 761 are checked for is:stale."));

        // Every open PR is loaded: the local filter's answer is exact.
        let view = all_open_view(Some("nothing-like-this"), Some(3));
        assert!(!footer_lines(&view).iter().any(|l| l.starts_with("Only")));
        assert!(markdown(&view, false)
            .contains("_No open PRs in this repository match nothing-like-this._"));
    }

    #[test]
    fn all_open_filters_github_answered_count_matches_and_say_so_when_empty() {
        // label: and author: went to GitHub, so the count is of matches and
        // nothing was checked only locally.
        let view = all_open_view(Some("label:bug author:bob"), Some(40));
        assert!(!view.remote.is_empty());
        assert!(view.local_only_terms().is_empty());
        assert_eq!(view.rows.len(), 1);
        assert!(
            status_line(&view).starts_with("acme/widgets · 3 of 40 match · 1 shown · "),
            "{}",
            status_line(&view)
        );
        assert!(!footer_lines(&view).iter().any(|l| l.starts_with("Only")));
        // repo: stays local, so it is named.
        let view = all_open_view(Some("author:bob repo:acme/widgets"), Some(40));
        assert_eq!(view.local_only_terms(), ["repo:acme/widgets"]);

        let mut fetch = fetch_of(Vec::new());
        fetch.total = Some(0);
        let empty = build(
            fetch,
            &AttentionState::empty(SnapshotNamespace::new("github.com", "me")),
            Mode::AllOpen,
            BoardScope::Repository("acme/widgets".into()),
            "me".into(),
            Filters {
                query: Some("label:\"area:editor\"".into()),
                ..Filters::default()
            },
            Utc::now(),
        );
        assert_eq!(board_json(&empty)["total"], 0);
        assert!(
            markdown(&empty, false)
                .contains("_No open PRs in this repository match label:\"area:editor\"._"),
            "{}",
            markdown(&empty, false)
        );
        assert!(table(&empty, 80, Paint::new(false), false)
            .contains("No open PRs in this repository match label:\"area:editor\"."));

        let mut fetch = fetch_of(Vec::new());
        fetch.total = Some(0);
        let unfiltered = build(
            fetch,
            &AttentionState::empty(SnapshotNamespace::new("github.com", "me")),
            Mode::AllOpen,
            BoardScope::Repository("acme/widgets".into()),
            "me".into(),
            Filters::default(),
            Utc::now(),
        );
        assert!(markdown(&unfiltered, false).contains("_No open PRs in this repository_"));
    }

    #[test]
    fn json_matches_the_published_schema() {
        use crate::schema_check::{assert_conforms, Schema};
        use prmarmot_core::github::rate_limit::RateLimitInfo;
        assert_conforms(Schema::Board, &board_json(&all_open_view(None, Some(761))));
        assert_conforms(Schema::Board, &board_json(&all_open_view(None, None)));
        for mode in [Mode::Authored, Mode::Review] {
            let mut view = sample_view(mode);
            assert_conforms(Schema::Board, &board_json(&view));
            view.attention_error = Some("attention state is unreadable".into());
            view.rate = Some(RateLimitInfo {
                limit: 5000,
                cost: 1,
                remaining: 4999,
                reset_at: "2026-09-16T12:00:00Z".into(),
            });
            view.scope = BoardScope::AllRepositories;
            assert_conforms(Schema::Board, &board_json(&view));
        }
    }

    #[test]
    fn json_lists_every_section_in_layout_order_with_full_facts() {
        let value = board_json(&sample_view(Mode::Authored));
        assert_eq!(value["schema"], BOARD_SCHEMA);
        assert_eq!(value["mode"], "authored");
        assert_eq!(
            value["scope"],
            json!({"type": "repository", "repo": "acme/widgets"})
        );
        let keys: Vec<&str> = value["sections"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["key"].as_str().unwrap())
            .collect();
        assert_eq!(keys, ["approved", "action", "snoozed"]);
        let action = &value["sections"][1]["prs"];
        // Stack layers come out in dependency order.
        let numbers: Vec<u64> = action
            .as_array()
            .unwrap()
            .iter()
            .map(|pr| pr["number"].as_u64().unwrap())
            .collect();
        assert_eq!(numbers, [10, 12, 13]);
        assert_eq!(action[0]["ci"], "fail");
        assert_eq!(action[0]["attention"]["watched"], true);
        assert_eq!(
            action[0]["blockers"],
            json!([{"type": "unresolved_comments", "count": 2}, {"type": "ci_failing"}])
        );
        assert_eq!(action[0]["note"], "CI failing · 2 unresolved");
        assert_eq!(
            action[0]["size"],
            json!({"band": "small", "additions": 30, "deletions": 12, "changed_files": 3})
        );
        assert_eq!(action[1]["size"], Value::Null);
        assert_eq!(value["sort"], "wait");
        assert_eq!(value["sections"][2]["prs"][0]["number"], 14);
        assert!(
            value["sections"][2]["prs"][0]["attention"]["snoozed"]["description"]
                .as_str()
                .unwrap()
                .starts_with("Snoozed until")
        );
    }

    #[test]
    fn markdown_escapes_cells_and_collapses_snoozed_by_default() {
        let md = markdown(&sample_view(Mode::Authored), false);
        assert!(md.starts_with("# My PRs · acme/widgets\n"), "{md}");
        assert!(md.contains("## Approved (1)"));
        assert!(md.contains("## Needs action (3)"));
        assert!(md.contains("Fix \\| pipes in titles"));
        assert!(
            md.contains("| CI failing · 2 unresolved · watched |"),
            "{md}"
        );
        assert!(md.contains("├─ 1/2 Change number 12"));
        assert!(md.contains("└─ 2/2 Change number 13"));
        assert!(md.contains("_1 snoozed PR hidden — pass `--snoozed` to include it._"));
        assert!(!md.contains("#14"));
        let shown = markdown(&sample_view(Mode::Authored), true);
        assert!(shown.contains("## Snoozed (1)"));
        assert!(shown.contains("[#14]"));
    }

    #[test]
    fn plain_table_fits_the_width_and_keeps_every_pr() {
        let view = sample_view(Mode::Authored);
        let text = table(&view, 100, Paint::new(false), false);
        for line in text.lines() {
            assert!(display_width(line) <= 100, "too wide: {line:?}");
        }
        assert!(
            text.starts_with("My PRs · acme/widgets · 5 PRs · synced "),
            "{text}"
        );
        assert!(text.contains("\nApproved 1\n"));
        assert!(text.contains("◎  #10"), "watched marker missing:\n{text}");
        assert!(text.contains("Stack #99 · 2 layers"));
        assert!(text.contains("Snoozed 1 · hidden, pass --snoozed to show"));
        assert!(text.ends_with("\n◎ watched\n"), "legend missing:\n{text}");
        assert!(!text.contains("\x1b["));
    }

    #[test]
    fn wide_terminals_show_whole_notes_and_long_refs_keep_their_number() {
        let mut long = row(7, Category::Action);
        long.repo = "oliver-kriska/headless-liveview-poc-with-a-very-long-name".into();
        long.blockers = vec![
            Blocker::MergeConflict,
            Blocker::CiFailing,
            Blocker::UnresolvedComments(5),
        ];
        let view = build(
            fetch_of(vec![long.clone(), row(8, Category::Await)]),
            &AttentionState::empty(SnapshotNamespace::new("github.com", "me")),
            Mode::Authored,
            BoardScope::AllRepositories,
            "me".into(),
            Filters::default(),
            Utc::now(),
        );
        let text = table(&view, 220, Paint::new(false), false);
        assert!(
            text.contains("merge conflict — rebase · CI failing · 5 unresolved\n"),
            "{text}"
        );
        assert!(text.contains("…#7  "), "{text}");
        for line in table(&view, 90, Paint::new(false), false).lines() {
            assert!(display_width(line) <= 90, "too wide: {line:?}");
        }
        assert_eq!(fit_pr_ref(&long, true, 20), "oliver-kriska/hea…#7");
        assert_eq!(display_width(&fit_pr_ref(&long, true, 20)), 20);
    }

    #[test]
    fn notes_say_how_long_a_pr_has_waited_and_warn_when_stale() {
        let waited = |number, since: &str| {
            let mut pr = row(number, Category::Todo);
            pr.note = "🔵 needs your review".into();
            pr.waiting_since = Some(since.into());
            pr
        };
        let mut conflicted = waited(3, "2026-09-01T06:00:00Z");
        conflicted.conflict = true;
        conflicted.note = "⚠️ has conflicts".into();
        let view = build(
            fetch_of(vec![
                waited(1, "2026-09-15T00:00:00Z"),
                waited(2, "2026-09-10T00:00:00Z"),
                conflicted,
                row(4, Category::Done),
            ]),
            &AttentionState::empty(SnapshotNamespace::new("github.com", "me")),
            Mode::Review,
            BoardScope::Repository("acme/widgets".into()),
            "me".into(),
            Filters::default(),
            Utc.with_ymd_and_hms(2026, 9, 15, 16, 30, 0).unwrap(),
        );
        let notes: Vec<(Tone, String)> = (0..4)
            .map(|ix| note_for(&view, ix))
            .map(|note| (note.tone, note.text))
            .collect();
        assert_eq!(
            notes,
            [
                (Tone::Warning, "needs your review · waiting 16h".into()),
                (
                    Tone::Warning,
                    "needs your review · waiting 5d (stale)".into()
                ),
                (Tone::Danger, "has conflicts · waiting 14d (stale)".into()),
                (Tone::Muted, "waiting on bob".into()),
            ]
        );
        let md = markdown(&view, false);
        assert!(md.contains("| needs your review · waiting 16h |"), "{md}");

        let mut view = view;
        view.filters.stale = true;
        assert_eq!(
            filter_line(&view).as_deref(),
            Some("Filter: stale (3d+ waiting) · 0 hidden")
        );
    }

    #[test]
    fn the_review_queue_shows_size_bands_and_can_list_the_smallest_first() {
        let sized = |number, lines| {
            let mut pr = row(number, Category::Todo);
            pr.note = "🔵 needs your review".into();
            pr.size = Some(ChangeSize {
                additions: lines,
                deletions: 0,
                changed_files: 1,
            });
            pr
        };
        let fetch = || fetch_of(vec![sized(1, 900), row(2, Category::Todo), sized(3, 20)]);
        let mut view = build(
            fetch(),
            &AttentionState::empty(SnapshotNamespace::new("github.com", "me")),
            Mode::Review,
            BoardScope::Repository("acme/widgets".into()),
            "me".into(),
            Filters::default(),
            Utc::now(),
        );
        assert_eq!(note_for(&view, 0).text, "needs your review · Large");
        assert_eq!(note_for(&view, 2).text, "needs your review · Small");
        let order = |view: &BoardView| -> Vec<u64> {
            view.layout(false)
                .iter()
                .filter_map(|item| match item {
                    LayoutItem::Row(ix) => Some(view.rows[*ix].number),
                    LayoutItem::Header { .. } => None,
                })
                .collect()
        };
        assert_eq!(order(&view), [1, 2, 3]);
        view.sort = Sort::Smallest;
        assert_eq!(order(&view), [3, 1, 2]);
        assert_eq!(board_json(&view)["sort"], "smallest");
        let text = table(&view, 120, Paint::new(false), false);
        assert!(
            text.lines().next().unwrap().ends_with(" · smallest first"),
            "{text}"
        );
        assert!(markdown(&view, false).contains("| needs your review · Small |"));

        // My PRs keep the band in JSON only.
        let mut mine = sized(4, 20);
        mine.category = Category::Await;
        mine.note = "🟡 waiting on bob".into();
        let view = build(
            fetch_of(vec![mine]),
            &AttentionState::empty(SnapshotNamespace::new("github.com", "me")),
            Mode::Authored,
            BoardScope::Repository("acme/widgets".into()),
            "me".into(),
            Filters::default(),
            Utc::now(),
        );
        assert_eq!(note_for(&view, 0).text, "waiting on bob");
        assert_eq!(
            board_json(&view)["sections"][0]["prs"][0]["size"]["band"],
            "small"
        );
    }

    #[test]
    fn view_titles_match_the_apps_switcher() {
        let repo = BoardScope::Repository("acme/widgets".into());
        let all = BoardScope::AllRepositories;
        assert_eq!(view_title(Mode::Authored, &repo, false), "My PRs");
        assert_eq!(view_title(Mode::Authored, &all, false), "Involving me");
        assert_eq!(view_title(Mode::Authored, &all, true), "My PRs");
        assert_eq!(view_title(Mode::Review, &all, false), "Review queue");
    }

    #[test]
    fn notes_follow_the_apps_tone_rules() {
        let mut pr = row(1, Category::Action);
        pr.blockers = vec![
            Blocker::NoReviewers {
                suggested: vec!["ann".into()],
            },
            Blocker::MergeConflict,
        ];
        let n = note(&pr);
        assert_eq!(n.tone, Tone::Danger);
        assert_eq!(n.text, "merge conflict — rebase · reviewers missing");
        pr.blockers = vec![Blocker::NoReviewers {
            suggested: vec!["ann".into(), "ben".into()],
        }];
        let n = note(&pr);
        assert_eq!(n.tone, Tone::Warning);
        assert_eq!(n.text, "assign ann + ben");
        let mut review = row(2, Category::Todo);
        review.conflict = true;
        assert_eq!(note(&review).tone, Tone::Danger);
        assert_eq!(note(&row(3, Category::Draft)).tone, Tone::Muted);
    }

    #[test]
    fn review_mode_uses_author_column_and_empty_copy() {
        let md = markdown(&sample_view(Mode::Review), false);
        assert!(md.contains("| PR | Title | Author | CI | Note |"));
        let empty = build(
            fetch_of(Vec::new()),
            &AttentionState::empty(SnapshotNamespace::new("github.com", "me")),
            Mode::Review,
            BoardScope::AllRepositories,
            "me".into(),
            Filters::default(),
            Utc::now(),
        );
        assert!(table(&empty, 80, Paint::new(false), false)
            .contains("No requested or available reviews in this result set"));
    }
}
