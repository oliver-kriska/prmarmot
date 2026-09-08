//! The board table: a `TableDelegate` over `Vec<BoardRow>` for
//! gpui-component's virtualized `Table`. Cells render the same glyph language
//! as the prototype's markdown dashboard (SKILL.md).
//!
//! Rows are grouped by category with a section-header row before each band
//! ("Needs action (14)"). Because published gpui-component 0.5.1 renders every
//! cell inside a fixed-width `overflow_hidden` box, a full-width header label
//! cannot live in a cell — it is drawn as an absolute overlay from `render_tr`
//! (the row container is NOT clipped) while that row's cells render empty.
//!
//! Nothing here may animate: the table sits idle between refreshes and any
//! continuous animation would defeat the idle-GPU half of the spike gate.

use gpui::prelude::FluentBuilder;
use gpui::{
    div, px, rems, App, ClickEvent, Context, Div, FontWeight, Hsla, InteractiveElement,
    IntoElement, MouseButton, ParentElement, Pixels, Stateful, StatefulInteractiveElement, Styled,
    Window,
};
use gpui_component::table::{Column, TableDelegate, TableState};
use gpui_component::tooltip::Tooltip;
use gpui_component::{h_flex, ActiveTheme};
use prboard_core::board::{Blocker, BoardRow, Category, Ci, Mode, ReviewState};

use crate::design::{CHIP_HEIGHT, CHIP_PAD_X, CHIP_RADIUS, STATUS_DOT};

/// One rendered line of the table: either a category section header or a PR
/// (an index into `rows`). Headers are pseudo-rows — `row()` returns `None`
/// for them, and keyboard selection bounces off them (see `app.rs`).
enum DisplayRow {
    Header {
        label: String,
        count: Option<usize>,
        detail: Option<String>,
    },
    Pr(usize),
}

/// Viewport-width buckets that drive the responsive column layout. Kept a
/// small closed set (not raw pixels) so the per-(mode, class) manual-resize
/// overrides in `app.rs` stay bounded (2 modes × 3 classes = 6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TableWidthClass {
    Compact,
    Medium,
    Wide,
}

/// Below this the layout is Compact (Labels column dropped).
const COMPACT_MAX: f32 = 1120.0;
/// At/above this the layout is Wide.
const WIDE_MIN: f32 = 1360.0;

impl TableWidthClass {
    pub fn from_width(width: f32) -> Self {
        if width < COMPACT_MAX {
            Self::Compact
        } else if width < WIDE_MIN {
            Self::Medium
        } else {
            Self::Wide
        }
    }
}

// Bounded fixed-column widths (px). Title and Note share the elastic remainder.
const PR_W: f32 = 92.0;
const CI_W: f32 = 68.0;
const UNRESOLVED_W: f32 = 52.0;
const LABELS_W: f32 = 96.0;
const REVIEW_W_COMPACT: f32 = 160.0;
const REVIEW_W: f32 = 190.0;
const AUTHOR_W_COMPACT: f32 = 96.0;
const AUTHOR_W: f32 = 116.0;
/// Title and Note never shrink below these; at the 900px floor the total still
/// fits without pushing Note offscreen.
const TITLE_MIN: f32 = 240.0;
const NOTE_MIN: f32 = 280.0;
/// Vertical-scrollbar + safety allowance subtracted from the viewport.
const SCROLLBAR_MARGIN: f32 = 24.0;
/// Title's share of the elastic remainder; Note keeps the larger rest.
const TITLE_FLEX_RATIO: f32 = 0.44;
/// Auto Title/Note widths quantize to this, so a resize drag only rebuilds
/// columns once per step instead of every pixel.
const QUANTUM: f32 = 16.0;
/// Placeholder viewport for the delegate's initial columns; `RootView::new`
/// immediately relayouts to the real window width.
const DEFAULT_VIEWPORT_WIDTH: f32 = 1280.0;

/// Horizontal padding gpui-component adds to every cell. The table is rendered
/// `.small()` (Size::Small → 6px each side); subtracted when eliding cell text.
const CELL_PAD_X: f32 = 12.0;
/// The cell font size, in rems. The `.small()` table applies `text_sm()` =
/// `rems(0.875)` to every cell, *overriding* the ambient 13px — so eliding must
/// measure at this size, not the ambient one, or long labels clip their "…".
/// Keep in lockstep with the `.small()`/`TABLE_TEXT_PX` choice in `app.rs`.
const CELL_FONT_REM: f32 = 0.875;
/// `gap_1` / `gap_1p5` in px at the default 16px rem — the inter-child gaps in
/// the title/note/review cells, needed for the elision width math.
const GAP_1: f32 = 4.0;
const GAP_1P5: f32 = 6.0;
/// A hair of slack so the appended "…" never lands under the cell's overflow
/// clip (our measured width can differ from the painted width by sub-pixels).
const ELIDE_SAFETY: f32 = 4.0;

/// The font size (px) cells actually render at — see `CELL_FONT_REM`.
fn cell_font_px(window: &Window) -> Pixels {
    rems(CELL_FONT_REM).to_pixels(window.rem_size())
}

/// Pixel width of `text` at the cell's rendered font size.
fn measure_width(window: &mut Window, text: &str) -> Pixels {
    if text.is_empty() {
        return px(0.);
    }
    let font_size = cell_font_px(window);
    let run = window.text_style().to_run(text.len());
    window
        .text_system()
        .layout_line(text, font_size, &[run], None)
        .width
}

/// Truncate `text` with a trailing "…" so it fits within `max_width`, at the
/// cell's rendered font size and using gpui's own line metrics. Done by hand
/// because gpui's in-cell `.truncate()` is inert inside gpui-component's
/// virtualized table: the row-measure pass shapes and caches the *untruncated*
/// line at an indefinite width, and the nowrap text cache then never
/// re-truncates at the real width. So we elide the string ourselves and render
/// a plain, already-fitting label.
fn elide(window: &mut Window, text: &str, max_width: Pixels) -> String {
    if text.is_empty() || max_width <= px(0.) {
        return text.to_string();
    }
    let font_size = cell_font_px(window);
    let font = window.text_style().font();
    let runs = vec![window.text_style().to_run(text.len())];
    window
        .text_system()
        .line_wrapper(font, font_size)
        .truncate_line(
            text.to_string().into(),
            max_width,
            "…",
            &runs,
            gpui::TruncateFrom::End,
        )
        .0
        .to_string()
}

/// The responsive column set for a mode at a given width class and viewport.
///
/// Human scanning order (critique #3): identity first (PR + draft badge), then
/// WHAT it is (Title), then health (CI), then the merged Review / Author +
/// Unresolved metadata, then Labels, then the Note. Fixed metadata columns get
/// bounded widths; Title and Note split the remaining viewport with Note kept
/// at least as wide as Title. Labels drop out in Compact. Pure — no GPUI
/// context — so the layout is unit-tested directly.
pub fn columns_for(mode: Mode, class: TableWidthClass, viewport_width: f32) -> Vec<Column> {
    let compact = class == TableWidthClass::Compact;
    let show_labels = !compact;
    let review_w = if compact { REVIEW_W_COMPACT } else { REVIEW_W };
    let author_w = if compact { AUTHOR_W_COMPACT } else { AUTHOR_W };
    let labels_w = if show_labels { LABELS_W } else { 0.0 };

    let fixed_sum = match mode {
        Mode::Authored => PR_W + CI_W + review_w + labels_w,
        Mode::Review => PR_W + CI_W + author_w + UNRESOLVED_W + labels_w,
    };
    // Elastic remainder, floored so Title/Note always meet their minimums.
    let flexible = (viewport_width - fixed_sum - SCROLLBAR_MARGIN).max(TITLE_MIN + NOTE_MIN);
    let mut title_w = ((flexible * TITLE_FLEX_RATIO / QUANTUM).round() * QUANTUM).max(TITLE_MIN);
    let note_w = (flexible - title_w).max(NOTE_MIN);
    if note_w < title_w {
        // Never let Title out-grow Note — Note is the product.
        title_w = note_w;
    }

    let col = |key: &'static str, name: &'static str, w: f32| Column::new(key, name).width(px(w));
    match mode {
        Mode::Authored => {
            let mut cols = vec![
                col("pr", "PR", PR_W),
                col("title", "Title", title_w),
                col("ci", "CI", CI_W),
                col("review", "Review", review_w),
            ];
            if show_labels {
                cols.push(col("labels", "Labels", LABELS_W));
            }
            cols.push(col("note", "Note", note_w));
            cols
        }
        Mode::Review => {
            let mut cols = vec![
                col("pr", "PR", PR_W),
                col("title", "Title", title_w),
                col("ci", "CI", CI_W),
                col("author", "Author", author_w),
                col("unresolved", "Unres", UNRESOLVED_W),
            ];
            if show_labels {
                cols.push(col("labels", "Labels", LABELS_W));
            }
            cols.push(col("note", "Note", note_w));
            cols
        }
    }
}

/// Case-insensitive AND search across the fields users scan in the board.
/// Filtering is local; it must not trigger GitHub requests on each keystroke.
pub fn matches_filter(row: &BoardRow, query: &str) -> bool {
    let text = format!(
        "#{} {} {} {} {} {}",
        row.number,
        row.title,
        row.author.as_deref().unwrap_or_default(),
        row.labels.join(" "),
        row.issue.as_deref().unwrap_or_default(),
        row.note
    )
    .to_lowercase();
    query
        .split_whitespace()
        .all(|word| text.contains(&word.to_lowercase()))
}

/// Full, unelided snapshot details; no secondary network request or hidden cache.
pub fn detail_text(row: &BoardRow) -> String {
    let mut lines = vec![
        strip_note_glyphs(&row.note),
        format!(
            "Author: {} · CI: {} · Unresolved threads: {}",
            row.author.as_deref().unwrap_or("unknown"),
            row.ci.as_str(),
            row.unresolved
        ),
        format!(
            "Requested reviewers: {}",
            if row.requested.is_empty() {
                "none".into()
            } else {
                row.requested.join(", ")
            }
        ),
        format!(
            "Reviews: {}",
            if row.reviews.is_empty() {
                "none".into()
            } else {
                row.reviews
                    .iter()
                    .map(|review| {
                        format!(
                            "{} — {}",
                            review.login.as_deref().unwrap_or("deleted user"),
                            review.state
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        ),
    ];
    if let Some(review) = &row.my_review {
        lines.push(format!("Your review: {review}"));
    }
    if !row.labels.is_empty() {
        lines.push(format!("Labels: {}", row.labels.join(", ")));
    }
    if let Some(issue) = &row.issue {
        lines.push(format!("Issue: {issue}"));
    }
    if let Some(stack) = &row.stack {
        lines.push(format!(
            "Stack #{} · Layer {} of {} · Base: {}",
            stack.number,
            stack
                .position
                .map(|p| p.to_string())
                .unwrap_or_else(|| "?".into()),
            stack.size,
            stack.base_ref_name
        ));
    }
    lines.push("Details reflect the loaded snapshot; refresh restarts pagination.".into());
    lines.join("\n")
}

pub struct BoardTableDelegate {
    rows: Vec<BoardRow>,
    display: Vec<DisplayRow>,
    columns: Vec<Column>,
    mode: Mode,
}

impl BoardTableDelegate {
    pub fn new(mode: Mode) -> Self {
        Self {
            rows: Vec::new(),
            display: Vec::new(),
            columns: columns_for(mode, TableWidthClass::Medium, DEFAULT_VIEWPORT_WIDTH),
            mode,
        }
    }

    /// Switch queue: change the sort/grouping mode and re-band the display.
    /// Columns are owned by `RootView` (they depend on the live window width),
    /// so it calls `set_columns` right after this.
    pub fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
        self.rebuild_display();
    }

    /// Replace the whole column set (responsive relayout or a mode switch).
    pub fn set_columns(&mut self, columns: Vec<Column>) {
        self.columns = columns;
    }

    /// Current column widths, in column order — used to snapshot a manual
    /// layout and to skip a no-op relayout.
    pub fn column_widths(&self) -> Vec<Pixels> {
        self.columns.iter().map(|c| c.width).collect()
    }

    /// Write runtime widths back into the delegate columns so the next
    /// `TableState::refresh` (which rebuilds from `Column.width`) preserves
    /// them. Rejects a width vector whose length does not match the active
    /// columns — a mode-switch race — returning `false`.
    pub fn set_column_widths(&mut self, widths: &[Pixels]) -> bool {
        if widths.len() != self.columns.len() {
            return false;
        }
        for (col, w) in self.columns.iter_mut().zip(widths) {
            col.width = *w;
        }
        true
    }

    pub fn set_rows(&mut self, rows: Vec<BoardRow>) {
        self.rows = rows;
        self.rebuild_display();
    }

    /// Interleave a section header before each contiguous category band. Rows
    /// arrive already sorted by category rank (core `derive_rows`), so a band
    /// is just a run of equal categories.
    fn rebuild_display(&mut self) {
        let mut display = Vec::with_capacity(self.rows.len() + 3);
        let mut i = 0;
        while i < self.rows.len() {
            let cat = self.rows[i].category;
            let start = i;
            while i < self.rows.len() && self.rows[i].category == cat {
                i += 1;
            }
            display.push(DisplayRow::Header {
                label: group_label(self.mode, cat).into(),
                count: Some(i - start),
                detail: None,
            });
            let mut emitted = std::collections::HashSet::new();
            for j in start..i {
                if let Some(stack) = &self.rows[j].stack {
                    if !emitted.insert(stack.number) {
                        continue;
                    }
                    let mut members: Vec<usize> = (start..i)
                        .filter(|&k| {
                            self.rows[k]
                                .stack
                                .as_ref()
                                .is_some_and(|s| s.number == stack.number)
                        })
                        .collect();
                    members.sort_by_key(|&k| {
                        self.rows[k]
                            .stack
                            .as_ref()
                            .and_then(|s| s.position)
                            .unwrap_or(u64::MAX)
                    });
                    display.push(DisplayRow::Header {
                        label: format!("Stack #{}", stack.number),
                        count: None,
                        detail: Some(if members.len() as u64 == stack.size {
                            format!("{} layers", stack.size)
                        } else {
                            format!("{} of {} layers shown", members.len(), stack.size)
                        }),
                    });
                    display.extend(members.into_iter().map(DisplayRow::Pr));
                } else {
                    display.push(DisplayRow::Pr(j));
                }
            }
        }
        self.display = display;
    }

    /// End the tree at the visible section boundary, even if other layers
    /// exist elsewhere. Standalone rows must never look like stack children.
    fn stack_ends_at(&self, display_ix: usize) -> bool {
        let Some(stack) = self.row(display_ix).and_then(|row| row.stack.as_ref()) else {
            return false;
        };
        self.row(display_ix + 1)
            .and_then(|row| row.stack.as_ref())
            .is_none_or(|next| next.number != stack.number)
    }

    /// The `BoardRow` at a display index, or `None` if it is a section header.
    pub fn row(&self, display_ix: usize) -> Option<&BoardRow> {
        match self.display.get(display_ix)? {
            DisplayRow::Pr(i) => self.rows.get(*i),
            DisplayRow::Header { .. } => None,
        }
    }

    /// True when the display index is a non-selectable section header.
    pub fn is_header(&self, display_ix: usize) -> bool {
        matches!(
            self.display.get(display_ix),
            Some(DisplayRow::Header { .. })
        )
    }

    /// The display index of the PR with this URL, if still present. Selection
    /// is tracked by URL (stable identity), not display index, so a background
    /// refresh that inserts/removes/reorders rows never silently reselects a
    /// different PR or a header.
    pub fn display_index_of_url(&self, url: &str) -> Option<usize> {
        self.display.iter().position(|d| match d {
            DisplayRow::Pr(i) => self.rows.get(*i).is_some_and(|r| r.url == url),
            DisplayRow::Header { .. } => false,
        })
    }

    pub fn display_len(&self) -> usize {
        self.display.len()
    }
}

/// Aggregate review-state word for the merged Review column ("✓ alice —
/// approved"). Requested-but-unreviewed is handled separately (no reviews yet).
fn review_state_word_aggregate(state: ReviewState) -> &'static str {
    match state {
        ReviewState::Approved => "approved",
        ReviewState::Changes => "changes requested",
        ReviewState::Commented => "commented",
        ReviewState::Waiting => "requested",
        ReviewState::None => "reviewed",
    }
}

/// The section label for a category within a mode. Action/Await differ from
/// Todo/Done even though they share a sort rank.
fn group_label(mode: Mode, cat: Category) -> &'static str {
    match (mode, cat) {
        (Mode::Authored, Category::Action) => "Needs action",
        (Mode::Authored, Category::Await) => "Awaiting review",
        (Mode::Review, Category::Todo) => "Requested from you",
        (Mode::Review, Category::Available) => "Available to review · no reviewer requested",
        (Mode::Review, Category::Done) => "Reviewed",
        (_, Category::Draft) => "Drafts",
        // Unreachable pairings (Action in Review etc.) — a calm fallback.
        _ => "Other",
    }
}

/// Calm reviewer-state glyph + its color token, per the design spec (§8).
/// Rendered BEFORE the login: on truncation the glyph is the information.
fn review_glyph(state: &str, theme: &gpui_component::theme::Theme) -> (&'static str, Hsla) {
    match state {
        "APPROVED" => ("✓", theme.success),
        "COMMENTED" => ("·", theme.muted_foreground),
        "CHANGES_REQUESTED" => ("±", theme.danger),
        // ✕, not "–": a bare dash reads as "nothing" — dismissed is an
        // invalidated review, which is information.
        "DISMISSED" => ("✕", theme.muted_foreground),
        _ => ("·", theme.muted_foreground),
    }
}

/// Notes come from core with the prototype's emoji language (the SKILL spec);
/// on this board a themed status dot carries that signal instead, so the
/// emoji are presentation noise — strip them, never change them in core.
fn strip_note_glyphs(note: &str) -> String {
    const GLYPHS: &[&str] = &[
        "⚠️ ", "🔴 ", "❌ ", "✋ ", "🟡 ", "🟢 ", "✅ ", "💬 ", "🔵 ",
    ];
    let mut s = note.to_string();
    for g in GLYPHS {
        s = s.replace(g, "");
    }
    s
}

fn status_dot(color: Hsla) -> Div {
    div()
        .size(px(STATUS_DOT))
        .rounded_full()
        .flex_shrink_0()
        .bg(color)
}

fn review_state_word(state: &str) -> &'static str {
    match state {
        "APPROVED" => "approved",
        "COMMENTED" => "commented",
        "CHANGES_REQUESTED" => "requested changes",
        "DISMISSED" => "dismissed",
        _ => "reviewed",
    }
}

/// The visual tone of a Note cell: it picks the dot color and whether the
/// primary phrase is alarm-colored. Severity is a *presentation* concern and
/// lives here, never in core (`prboard_core::board::Blocker` carries only
/// facts). See the rendering rules in the note-hierarchy plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NoteTone {
    /// Exceptional blocker — merge conflict / CI failure / changes requested.
    /// Red dot, red primary phrase; it should interrupt the scan.
    Danger,
    /// Routine action needed — reviewers to assign, comments to resolve, a
    /// review still owed. Amber dot, muted text: visible but peripheral, so a
    /// column of them never forms a red wall.
    Warning,
    /// Merged-path good news — approved / awaiting after approval. Green dot.
    Success,
    /// Completed but neutral — you already reviewed, nothing outstanding on
    /// you. A muted dot, muted text.
    Routine,
    /// Draft / inactive: no dot, dim text.
    Muted,
}

/// A Note cell decomposed for exception-first rendering: one emphasized
/// `primary` phrase, an optional inline `remedy`, and muted `context` facts so
/// no blocker disappears into the tooltip alone. Pure data (no GPUI types), so
/// it is unit-tested without a window.
#[derive(Debug, Clone, PartialEq, Eq)]
struct NotePresentation {
    tone: NoteTone,
    primary: String,
    remedy: Option<String>,
    context: Vec<String>,
    /// The full, glyph-stripped canonical note — always the hover tooltip.
    tooltip: String,
}

/// Presentation priority — deliberately different from the canonical note
/// order: the most operationally urgent blocker is shown first and colored.
fn blocker_rank(blocker: &Blocker) -> u8 {
    match blocker {
        Blocker::MergeConflict => 0,
        Blocker::CiFailing => 1,
        Blocker::ChangesRequested => 2,
        Blocker::UnresolvedComments(_) => 3,
        Blocker::NoReviewers { .. } => 4,
    }
}

/// Merge conflict / CI failure / changes-requested interrupt the scan (danger);
/// unresolved comments and missing reviewers are routine follow-up (warning).
fn is_exceptional(blocker: &Blocker) -> bool {
    matches!(
        blocker,
        Blocker::MergeConflict | Blocker::CiFailing | Blocker::ChangesRequested
    )
}

/// The blocker as the emphasized primary phrase (+ optional muted remedy).
fn blocker_primary(blocker: &Blocker) -> (String, Option<String>) {
    match blocker {
        Blocker::MergeConflict => ("merge conflict".into(), Some("rebase".into())),
        Blocker::CiFailing => ("CI failing".into(), None),
        Blocker::ChangesRequested => ("changes requested".into(), None),
        Blocker::UnresolvedComments(n) => (
            format!("resolve {n} comment{}", if *n == 1 { "" } else { "s" }),
            None,
        ),
        Blocker::NoReviewers { suggested } => {
            if suggested.is_empty() {
                ("assign reviewers".into(), None)
            } else {
                (format!("assign {}", suggested.join(" + ")), None)
            }
        }
    }
}

/// The blocker as a compact muted context fact (shown when a higher-priority
/// blocker is the primary), so it stays visible on the row, not only on hover.
fn blocker_context(blocker: &Blocker) -> String {
    match blocker {
        Blocker::MergeConflict => "merge conflict".into(),
        Blocker::CiFailing => "CI failing".into(),
        Blocker::ChangesRequested => "changes requested".into(),
        Blocker::UnresolvedComments(n) => format!("{n} unresolved"),
        Blocker::NoReviewers { .. } => "reviewers missing".into(),
    }
}

/// Split a note into a primary phrase and an optional " — " remedy, for the
/// review-queue exceptional notes ("CI red — maybe wait for green").
fn split_remedy(text: &str) -> (String, Option<String>) {
    match text.split_once(" — ") {
        Some((head, tail)) => (head.to_string(), Some(tail.to_string())),
        None => (text.to_string(), None),
    }
}

/// Exception-first decomposition of an authored **Action** row: the highest
/// presentation-priority blocker becomes the primary (danger-colored when it is
/// exceptional), and every remaining blocker becomes a muted context fact in
/// priority order — nothing is dropped.
fn action_presentation(row: &BoardRow, tooltip: String) -> NotePresentation {
    let mut ranked: Vec<&Blocker> = row.blockers.iter().collect();
    ranked.sort_by_key(|b| blocker_rank(b));
    let Some((primary_blocker, rest)) = ranked.split_first() else {
        // An Action row always carries >=1 blocker; degrade calmly if not.
        return NotePresentation {
            tone: NoteTone::Warning,
            primary: tooltip.clone(),
            remedy: None,
            context: Vec::new(),
            tooltip,
        };
    };
    let tone = if is_exceptional(primary_blocker) {
        NoteTone::Danger
    } else {
        NoteTone::Warning
    };
    let (primary, remedy) = blocker_primary(primary_blocker);
    let context = rest.iter().map(|b| blocker_context(b)).collect();
    NotePresentation {
        tone,
        primary,
        remedy,
        context,
        tooltip,
    }
}

/// A one-phrase note: the whole (stripped) note as the primary, no remedy or
/// context. Used for the calm await/done/draft/routine states.
fn plain_note(tone: NoteTone, tooltip: String) -> NotePresentation {
    NotePresentation {
        tone,
        primary: tooltip.clone(),
        remedy: None,
        context: Vec::new(),
        tooltip,
    }
}

/// Turn a row into its calm-then-exception Note presentation. Pure — the
/// rendering in `render_td` only maps tone → colors. Each arm moves `tooltip`
/// exactly once (the arms are mutually exclusive), so no clone is needed.
fn note_presentation(row: &BoardRow) -> NotePresentation {
    let tooltip = strip_note_glyphs(&row.note);
    match row.category {
        Category::Action => action_presentation(row, tooltip),
        // Review queue: a red health signal interrupts; otherwise it is a
        // routine "please review", warned but calm — never a danger wall.
        Category::Todo | Category::Available => {
            if row.ci == Ci::Fail || row.conflict {
                let (primary, remedy) = split_remedy(&tooltip);
                NotePresentation {
                    tone: NoteTone::Danger,
                    primary,
                    remedy,
                    context: Vec::new(),
                    tooltip,
                }
            } else {
                plain_note(NoteTone::Warning, tooltip)
            }
        }
        Category::Await => plain_note(NoteTone::Success, tooltip),
        // "You approved" is good news; "you commented / requested changes" is
        // neutral — the ball is on the author, nothing is wrong.
        Category::Done => {
            if row.my_review.as_deref() == Some("APPROVED") {
                plain_note(NoteTone::Success, tooltip)
            } else {
                plain_note(NoteTone::Routine, tooltip)
            }
        }
        Category::Draft => plain_note(NoteTone::Muted, tooltip),
    }
}

impl TableDelegate for BoardTableDelegate {
    fn columns_count(&self, _cx: &App) -> usize {
        self.columns.len()
    }

    fn rows_count(&self, _cx: &App) -> usize {
        self.display.len()
    }

    fn column(&self, col_ix: usize, _cx: &App) -> Column {
        self.columns[col_ix].clone()
    }

    fn render_tr(
        &mut self,
        row_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> Stateful<Div> {
        let tr = div().id(("board-row", row_ix));
        let theme = cx.theme();
        match self.display.get(row_ix) {
            // Section header: a subtle band with a stronger top rule, and the
            // label drawn as an absolute overlay (cells render empty on this
            // row so nothing paints over it — see the module note).
            Some(DisplayRow::Header {
                label,
                count,
                detail,
            }) => tr
                .relative()
                .bg(theme.background)
                .when(count.is_some(), |header| {
                    header
                        .bg(theme.secondary)
                        .border_t_1()
                        .border_color(theme.border)
                })
                .child(
                    h_flex()
                        .absolute()
                        .left(if count.is_some() {
                            px(crate::design::HEADER_PAD_X)
                        } else {
                            self.columns[0].width + px(6.)
                        })
                        .top_0()
                        .bottom_0()
                        .items_center()
                        .gap_1p5()
                        .child(
                            div()
                                .text_size(px(if count.is_some() { 12. } else { 11. }))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme.secondary_foreground)
                                .child(label.clone()),
                        )
                        .when_some(*count, |header, count| {
                            header.child(
                                div()
                                    .px(px(6.))
                                    .rounded(px(4.))
                                    .bg(theme.background)
                                    .text_size(px(11.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.muted_foreground)
                                    .child(count.to_string()),
                            )
                        })
                        .when_some(detail.clone(), |header, detail| {
                            header.child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(theme.muted_foreground)
                                    .child(format!("· {detail}")),
                            )
                        }),
                ),
            // No per-row tint: the "Needs action" section header, the red note
            // dot, and the red problem phrase already signal urgency three
            // times over. A red row band on top of that dominates the screen
            // and flattens individual CI-fail / conflict rows into one alarm
            // block (design review, 2026-07-24). Zebra striping stays; state
            // lives in the Note cell.
            Some(DisplayRow::Pr(_)) => tr.when(self.stack_ends_at(row_ix), |row| {
                row.border_b_1().border_color(theme.border)
            }),
            None => tr,
        }
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        // Header rows carry no cell content — the label is an overlay from
        // render_tr; empty (transparent) cells let it show through.
        let row = match self.display.get(row_ix) {
            Some(DisplayRow::Pr(i)) => match self.rows.get(*i) {
                Some(r) => r,
                None => return div().into_any_element(),
            },
            _ => return div().into_any_element(),
        };
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        // Draft rows dim their text — inactive, not merely different.
        let dim = row.draft;

        let cell = match self.columns[col_ix].key.as_ref() {
            "pr" => {
                // Single-click link (critique #5): the blue #number opens the
                // PR. A compact "draft" badge replaces the old always-"ready"
                // Status column.
                let url = row.url.clone();
                let number = h_flex()
                    .id(("pr-link", row_ix))
                    .cursor_pointer()
                    .text_color(if dim { muted } else { theme.link })
                    .hover(|this| this.underline())
                    .child(format!("#{}", row.number))
                    // Stop the click bubbling to the row (which would also open
                    // the PR / start a double-click) and open only on a single
                    // click — the gpui-component `Link` pattern (critique #3).
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |_, e: &ClickEvent, _, cx| {
                        if e.click_count() == 1 {
                            cx.open_url(&url);
                        }
                    }));
                let mut cell = h_flex().gap_1().items_center().child(number);
                if row.draft {
                    cell = cell.child(
                        div()
                            .px(px(4.))
                            .rounded(px(CHIP_RADIUS))
                            .bg(theme.muted)
                            .border_1()
                            .border_color(theme.border)
                            .text_size(px(10.))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(muted)
                            .child("draft"),
                    );
                }
                return cell.into_any_element();
            }
            "labels" => {
                if row.labels.is_empty() {
                    // Blank, not a dash: three vertical bands of "—" start to
                    // read as data (design review). Dashes stay only where
                    // "none" is a meaningful status (CI).
                    div()
                } else {
                    // Chips; "bug" is the loud one. Cap the visible count —
                    // the tooltip carries the full list.
                    const VISIBLE: usize = 2;
                    let full = row.labels.join(", ");
                    // "bug" must never hide behind the +n overflow.
                    let mut ordered: Vec<String> = row.labels.clone();
                    ordered.sort_by_key(|l| l != "bug");
                    // One neutral chip style (spec §7) — GitHub's arbitrary
                    // label hues would out-shout the status system. 🐛 is the
                    // single permitted emoji: semantic, not decorative.
                    let chip_base = || {
                        h_flex()
                            .h(px(CHIP_HEIGHT))
                            .px(px(CHIP_PAD_X))
                            .items_center()
                            .rounded(px(CHIP_RADIUS))
                            .bg(theme.muted)
                            .border_1()
                            .border_color(theme.border)
                            .text_size(px(11.))
                            .font_weight(FontWeight::MEDIUM)
                            .whitespace_nowrap()
                    };
                    let mut chips = h_flex().gap_1().overflow_hidden();
                    for label in ordered.iter().take(VISIBLE) {
                        let text = if label == "bug" {
                            format!("🐛 {label}")
                        } else {
                            label.clone()
                        };
                        chips = chips.child(
                            chip_base()
                                .text_color(theme.secondary_foreground)
                                .child(text),
                        );
                    }
                    if row.labels.len() > VISIBLE {
                        chips = chips.child(
                            chip_base()
                                .text_color(muted)
                                .child(format!("+{}", row.labels.len() - VISIBLE)),
                        );
                    }
                    return chips
                        .id(("labels", row_ix))
                        .tooltip(move |window, cx| Tooltip::new(full.clone()).build(window, cx))
                        .into_any_element();
                }
            }
            // The calm rule (spec §8): bad states get colored text, good
            // states get only a colored dot with muted text.
            "ci" => match row.ci {
                Ci::Pass => h_flex()
                    .gap_1p5()
                    .items_center()
                    .child(status_dot(theme.success))
                    .child(div().text_color(muted).child("pass")),
                Ci::Fail => h_flex()
                    .gap_1p5()
                    .items_center()
                    .child(status_dot(theme.danger))
                    .child(div().text_color(theme.danger).child("fail")),
                Ci::Running => h_flex()
                    .gap_1p5()
                    .items_center()
                    .child(status_dot(theme.warning))
                    .child(div().text_color(muted).child("running")),
                Ci::None => h_flex().child(div().text_color(muted.opacity(0.5)).child("—")),
            },
            "author" => div().child(row.author.clone().unwrap_or_else(|| "?".into())),
            "unresolved" => {
                if row.unresolved > 0 {
                    div()
                        .text_color(theme.warning)
                        .child(row.unresolved.to_string())
                } else {
                    div()
                }
            }
            "review" => {
                // Merged Requested + Reviewed by: completed reviews win (they
                // supersede a pending request); else show who's requested; else
                // blank. Most cells were empty split across two columns — this
                // recovers ~150px for Title/Note. Glyph-first so state survives
                // truncation.
                if !row.reviews.is_empty() {
                    let mut cell = h_flex().gap_2().items_center().overflow_hidden();
                    for r in &row.reviews {
                        let (glyph, color) = review_glyph(&r.state, theme);
                        cell = cell.child(
                            h_flex()
                                .gap_1()
                                .items_center()
                                .whitespace_nowrap()
                                .child(
                                    div()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(color)
                                        .child(glyph),
                                )
                                .child(r.login.clone().unwrap_or_else(|| "?".into())),
                        );
                    }
                    cell = cell.child(div().flex_shrink_0().text_color(muted).child(format!(
                        "— {}",
                        review_state_word_aggregate(row.review_state)
                    )));
                    let full = row
                        .reviews
                        .iter()
                        .map(|r| {
                            format!(
                                "{} {}",
                                r.login.as_deref().unwrap_or("?"),
                                review_state_word(&r.state)
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(" · ");
                    return cell
                        .id(("review", row_ix))
                        .tooltip(move |window, cx| Tooltip::new(full.clone()).build(window, cx))
                        .into_any_element();
                } else if !row.requested.is_empty() {
                    let names = row.requested.join(", ");
                    let full = format!("requested: {names}");
                    // Elide the names to the room left by the "→" and the
                    // "— requested" suffix (both flex_shrink_0), with a real "…".
                    let arrow_w = measure_width(window, "→");
                    let suffix_w = measure_width(window, "— requested");
                    let col_w = self.columns[col_ix].width;
                    let avail =
                        col_w - arrow_w - suffix_w - px(CELL_PAD_X + GAP_1 + GAP_1 + ELIDE_SAFETY);
                    let names = elide(window, &names, avail);
                    return h_flex()
                        .w_full()
                        .gap_1()
                        .items_center()
                        .overflow_hidden()
                        .child(div().flex_shrink_0().text_color(muted).child("→"))
                        .child(div().flex_shrink_0().child(names))
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_color(muted.opacity(0.7))
                                .child("— requested"),
                        )
                        .id(("review", row_ix))
                        .tooltip(move |window, cx| Tooltip::new(full.clone()).build(window, cx))
                        .into_any_element();
                } else {
                    div().text_color(muted).child("Not requested")
                }
            }
            "title" => {
                let mut full = match &row.issue {
                    Some(issue) => format!("{issue} · {}", row.title),
                    None => row.title.clone(),
                };
                let stack_prefix = row.stack.as_ref().map(|s| {
                    let position = s.position.map(|p| p.to_string()).unwrap_or_else(|| "?".into());
                    full.push_str(&format!("\nStack #{} · layer {}/{} · base {}. Only matching PRs are shown; layers may be in other sections.", s.number, position, s.size, s.base_ref_name));
                    let branch = if self.stack_ends_at(row_ix) { "└─" } else { "├─" };
                    format!("{branch} {position}/{}", s.size)
                });
                let stack_w = stack_prefix
                    .as_deref()
                    .map(|s| measure_width(window, s) + px(GAP_1))
                    .unwrap_or(px(0.));
                let tag_color = if dim { muted } else { theme.accent_foreground };
                // Elide the title to the width the flexible region actually has
                // (column minus the issue tag, gap and cell padding). A real
                // "…", not a mid-word clip — the affordance that says "there's
                // more, hover" (critique #3). Done by hand via `elide` because
                // gpui's in-cell `.truncate()` is inert in this table.
                let col_w = self.columns[col_ix].width;
                let issue_w = row
                    .issue
                    .as_deref()
                    .map(|s| measure_width(window, s))
                    .unwrap_or(px(0.));
                let gap = if row.issue.is_some() { GAP_1 } else { 0.0 };
                let avail = col_w - issue_w - stack_w - px(CELL_PAD_X + gap + ELIDE_SAFETY);
                let title = elide(window, &row.title, avail);
                let inner = match (&row.issue, &row.issue_url) {
                    // A linked issue is a single-click link of its own
                    // (critique #5): the tag opens the tracker, not the PR.
                    (Some(issue), Some(issue_url)) => {
                        let issue_url = issue_url.clone();
                        h_flex()
                            .w_full()
                            .gap_1()
                            .overflow_hidden()
                            .when(dim, |t| t.text_color(muted))
                            .child(
                                h_flex()
                                    .id(("issue-link", row_ix))
                                    .flex_shrink_0()
                                    .cursor_pointer()
                                    .text_color(if dim { muted } else { theme.link })
                                    .hover(|this| this.underline())
                                    .child(issue.clone())
                                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                        cx.stop_propagation()
                                    })
                                    .on_click(cx.listener(move |_, e: &ClickEvent, _, cx| {
                                        if e.click_count() == 1 {
                                            cx.open_url(&issue_url)
                                        }
                                    })),
                            )
                            .child(div().flex_shrink_0().child(title))
                    }
                    (Some(issue), None) => h_flex()
                        .w_full()
                        .gap_1()
                        .overflow_hidden()
                        .when(dim, |t| t.text_color(muted))
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_color(tag_color)
                                .child(issue.clone()),
                        )
                        .child(div().flex_shrink_0().child(title)),
                    (None, _) => h_flex().w_full().overflow_hidden().child(
                        div()
                            .flex_shrink_0()
                            .when(dim, |t| t.text_color(muted))
                            .child(title),
                    ),
                };
                return h_flex()
                    .w_full()
                    .gap_1()
                    .overflow_hidden()
                    .when_some(stack_prefix, |this, prefix| {
                        this.child(div().flex_shrink_0().text_color(muted).child(prefix))
                    })
                    .child(inner)
                    .id(("title", row_ix))
                    .tooltip(move |window, cx| Tooltip::new(full.clone()).build(window, cx))
                    .into_any_element();
            }
            "note" => {
                // Exception-first Note (note-hierarchy plan): the dot + a single
                // emphasized primary phrase carry the row's worst blocker;
                // every other blocker trails as muted context so nothing hides
                // in the tooltip. Only genuinely exceptional blockers get red —
                // routine "assign reviewers" / "resolve N" rows are amber-muted,
                // so a column of them no longer reads as one red wall.
                let NotePresentation {
                    tone,
                    primary,
                    remedy,
                    context,
                    tooltip,
                } = note_presentation(row);
                let (dot_color, primary_color) = match tone {
                    NoteTone::Danger => (Some(theme.danger), theme.danger),
                    NoteTone::Warning => (Some(theme.warning), muted),
                    NoteTone::Success => (Some(theme.success), muted),
                    NoteTone::Routine => (Some(muted), muted),
                    NoteTone::Muted => (None, muted),
                };
                // The muted tail: the primary's remedy, then the remaining
                // blockers as context, in presentation-priority order.
                let mut tail = String::new();
                if let Some(remedy) = &remedy {
                    tail.push_str(" — ");
                    tail.push_str(remedy);
                }
                for fact in &context {
                    tail.push_str(" · ");
                    tail.push_str(fact);
                }
                // pr_2: the terminal column needs an optical margin the 6px
                // cell pad doesn't give (critique #4).
                let mut cell = h_flex()
                    .w_full()
                    .gap_1p5()
                    .items_center()
                    .overflow_hidden()
                    .pr_2();
                if let Some(color) = dot_color {
                    cell = cell.child(status_dot(color));
                }
                // Primary never truncates away; the muted tail absorbs the
                // ellipsis when the row is narrow. Elide the tail to the space
                // the dot + primary leave (a real "…", see `elide`).
                let dot_region = if dot_color.is_some() {
                    STATUS_DOT + GAP_1P5
                } else {
                    0.0
                };
                let primary_w = measure_width(window, &primary);
                cell = cell.child(
                    div()
                        .flex_shrink_0()
                        .text_color(primary_color)
                        .child(primary),
                );
                if !tail.is_empty() {
                    let col_w = self.columns[col_ix].width;
                    let avail = col_w
                        - primary_w
                        - px(CELL_PAD_X + 8.0 + dot_region + GAP_1P5 + ELIDE_SAFETY);
                    let tail = elide(window, &tail, avail);
                    cell = cell.child(div().flex_shrink_0().text_color(muted).child(tail));
                }
                return cell
                    .id(("note", row_ix))
                    .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                    .into_any_element();
            }
            _ => div(),
        };
        cell.into_any_element()
    }

    fn render_last_empty_col(
        &mut self,
        _window: &mut Window,
        _cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        // The default renders a 12px filler that reads as a stray empty
        // column header after Note. The Note column is elastic; no filler.
        div()
    }

    fn render_empty(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        // Queue-specific empty copy (critique #6): each scope says its own
        // "nothing here" so an empty Review queue never reads as no authored
        // PRs. Text only — the default empty view pulls an SVG from an asset
        // bundle this app does not ship.
        let msg = match self.mode {
            Mode::Authored => "You have no open PRs",
            Mode::Review => "No requested or available reviews in this result set",
        };
        h_flex()
            .size_full()
            .justify_center()
            .text_color(cx.theme().muted_foreground)
            .child(msg)
    }
}

#[cfg(test)]
mod tests {
    //! Pure display-model tests — no GPUI context. These guard the section
    //! grouping and the URL-identity selection the design review flagged.
    //! They run under `cargo test` (which compiles the GPUI binary), not the
    //! fast core-only `make check`.
    use super::*;

    fn row(number: u64, category: Category) -> BoardRow {
        BoardRow {
            number,
            url: format!("https://github.com/acme/widgets/pull/{number}"),
            title: format!("PR {number}"),
            issue: None,
            issue_url: None,
            author: None,
            stack: None,
            queue_provenance: None,
            draft: matches!(category, Category::Draft),
            category,
            bug: false,
            labels: Vec::new(),
            ci: Ci::Pass,
            conflict: false,
            review_decision: None,
            review_state: ReviewState::None,
            requested: Vec::new(),
            reviews: Vec::new(),
            my_review: None,
            unresolved: 0,
            blockers: Vec::new(),
            created_at: String::new(),
            note: String::new(),
        }
    }

    /// An authored Action row with an explicit blocker list + canonical note,
    /// for the pure note-presentation tests.
    fn action_row(blockers: Vec<Blocker>, note: &str) -> BoardRow {
        let mut r = row(1, Category::Action);
        r.blockers = blockers;
        r.note = note.to_string();
        r
    }

    #[test]
    fn no_reviewers_only_is_warning_and_muted() {
        let p = note_presentation(&action_row(
            vec![Blocker::NoReviewers {
                suggested: vec!["alice".into(), "bob".into()],
            }],
            "⚠️ no reviewers — assign alice + bob",
        ));
        assert_eq!(p.tone, NoteTone::Warning);
        assert_eq!(p.primary, "assign alice + bob");
        assert!(p.remedy.is_none());
        assert!(p.context.is_empty());
    }

    #[test]
    fn no_suggested_reviewers_falls_back_to_generic() {
        let p = note_presentation(&action_row(
            vec![Blocker::NoReviewers { suggested: vec![] }],
            "⚠️ no reviewers",
        ));
        assert_eq!(p.tone, NoteTone::Warning);
        assert_eq!(p.primary, "assign reviewers");
    }

    #[test]
    fn conflict_outranks_reviewers_and_keeps_them_as_context() {
        let p = note_presentation(&action_row(
            vec![
                Blocker::NoReviewers {
                    suggested: vec!["alice".into(), "bob".into()],
                },
                Blocker::MergeConflict,
            ],
            "⚠️ no reviewers — assign alice + bob · 🔴 merge conflict — rebase",
        ));
        assert_eq!(p.tone, NoteTone::Danger);
        assert_eq!(p.primary, "merge conflict");
        assert_eq!(p.remedy.as_deref(), Some("rebase"));
        assert_eq!(p.context, vec!["reviewers missing".to_string()]);
    }

    #[test]
    fn every_blocker_is_represented_across_primary_and_context() {
        let p = note_presentation(&action_row(
            vec![
                Blocker::NoReviewers {
                    suggested: vec!["alice".into()],
                },
                Blocker::MergeConflict,
                Blocker::CiFailing,
                Blocker::ChangesRequested,
                Blocker::UnresolvedComments(3),
            ],
            "canonical",
        ));
        assert_eq!(p.tone, NoteTone::Danger);
        assert_eq!(p.primary, "merge conflict");
        assert_eq!(
            p.context,
            vec![
                "CI failing".to_string(),
                "changes requested".to_string(),
                "3 unresolved".to_string(),
                "reviewers missing".to_string(),
            ]
        );
    }

    #[test]
    fn ci_failure_outranks_unresolved_comments() {
        // Listed unresolved-first, but CI (rank 1) beats unresolved (rank 3).
        let p = note_presentation(&action_row(
            vec![Blocker::UnresolvedComments(2), Blocker::CiFailing],
            "canonical",
        ));
        assert_eq!(p.tone, NoteTone::Danger);
        assert_eq!(p.primary, "CI failing");
        assert_eq!(p.context, vec!["2 unresolved".to_string()]);
    }

    #[test]
    fn routine_review_queue_item_is_warning_not_danger() {
        let mut r = row(5, Category::Todo);
        r.note = "🔵 needs your review".into();
        let p = note_presentation(&r);
        assert_eq!(p.tone, NoteTone::Warning);
        assert_eq!(p.primary, "needs your review");
    }

    #[test]
    fn review_queue_with_bad_ci_or_conflict_is_danger() {
        let mut r = row(6, Category::Todo);
        r.ci = Ci::Fail;
        r.note = "⚠️ CI red — maybe wait for green".into();
        let p = note_presentation(&r);
        assert_eq!(p.tone, NoteTone::Danger);
        assert_eq!(p.primary, "CI red");
        assert_eq!(p.remedy.as_deref(), Some("maybe wait for green"));

        let mut r2 = row(7, Category::Todo);
        r2.conflict = true;
        r2.note = "⚠️ has conflicts".into();
        assert_eq!(note_presentation(&r2).tone, NoteTone::Danger);
    }

    #[test]
    fn tooltip_is_the_full_stripped_canonical_note() {
        let p = note_presentation(&action_row(
            vec![
                Blocker::NoReviewers {
                    suggested: vec!["alice".into(), "bob".into()],
                },
                Blocker::CiFailing,
                Blocker::UnresolvedComments(3),
            ],
            "⚠️ no reviewers — assign alice + bob · ❌ CI failing · 🟡 3 unresolved comments",
        ));
        assert_eq!(
            p.tooltip,
            "no reviewers — assign alice + bob · CI failing · 3 unresolved comments"
        );
        // And the visible row still surfaces every fact: CI primary, the rest muted.
        assert_eq!(p.primary, "CI failing");
        assert_eq!(
            p.context,
            vec!["3 unresolved".to_string(), "reviewers missing".to_string()]
        );
    }

    #[test]
    fn headers_group_contiguous_categories_with_counts() {
        let mut d = BoardTableDelegate::new(Mode::Authored);
        d.set_rows(vec![
            row(1, Category::Action),
            row(2, Category::Action),
            row(3, Category::Await),
            row(4, Category::Draft),
        ]);
        // 3 headers + 4 PRs.
        assert_eq!(d.display_len(), 7);
        assert!(d.is_header(0) && !d.is_header(1) && !d.is_header(2));
        assert!(d.is_header(3) && !d.is_header(4));
        assert!(d.is_header(5) && !d.is_header(6));
        match &d.display[0] {
            DisplayRow::Header { label, count, .. } => {
                assert_eq!(*label, "Needs action");
                assert_eq!(*count, Some(2));
            }
            _ => panic!("expected a header at 0"),
        }
    }

    #[test]
    fn empty_groups_emit_no_header() {
        let mut d = BoardTableDelegate::new(Mode::Authored);
        d.set_rows(vec![row(1, Category::Await)]);
        // Only "Awaiting review" — no empty Action/Draft headers.
        assert_eq!(d.display_len(), 2);
        assert!(d.is_header(0) && !d.is_header(1));
    }

    #[test]
    fn stacks_keep_layer_order_without_mixing_action_sections() {
        let stacked = |number, position, category| {
            let mut r = row(number, category);
            r.stack = Some(prboard_core::board::StackInfo {
                number: 70,
                size: 3,
                base_ref_name: "main".into(),
                position: Some(position),
            });
            r
        };
        let mut d = BoardTableDelegate::new(Mode::Authored);
        d.set_rows(vec![
            stacked(9, 3, Category::Action),
            row(8, Category::Action),
            stacked(7, 2, Category::Action),
            stacked(6, 1, Category::Await),
        ]);
        let numbers: Vec<_> = (0..d.display_len())
            .filter_map(|i| d.row(i).map(|r| r.number))
            .collect();
        assert_eq!(numbers, vec![7, 9, 8, 6]);
        assert_eq!(d.display.iter().filter(|d| matches!(d, DisplayRow::Header { label, .. } if label.starts_with("Stack #70"))).count(), 2);
        assert!(
            matches!(&d.display[1], DisplayRow::Header { detail: Some(detail), .. } if detail == "2 of 3 layers shown")
        );
        let ix = d
            .display_index_of_url("https://github.com/acme/widgets/pull/7")
            .unwrap();
        assert_eq!(d.row(ix).unwrap().number, 7);
        assert!(!d.stack_ends_at(ix));
        assert!(d.stack_ends_at(ix + 1)); // closes before standalone #8
        assert!(!d.stack_ends_at(ix + 2)); // standalone, not a stack child
        assert!(d.stack_ends_at(d.display_len() - 1)); // final partial layer
        assert!(!d.stack_ends_at(0)); // category header

        d.set_rows(vec![
            stacked(7, 1, Category::Action),
            stacked(8, 2, Category::Action),
            stacked(9, 3, Category::Action),
        ]);
        assert!(
            matches!(&d.display[1], DisplayRow::Header { detail: Some(detail), .. } if detail == "3 layers")
        );
        assert!(d.stack_ends_at(d.display_len() - 1));
    }

    #[test]
    fn available_reviews_have_a_distinct_section_and_calm_note() {
        assert_ne!(
            group_label(Mode::Review, Category::Todo),
            group_label(Mode::Review, Category::Available)
        );
        let mut r = row(1, Category::Available);
        r.note = "available for review".into();
        assert_eq!(note_presentation(&r).tone, NoteTone::Warning);
    }

    #[test]
    fn filter_matches_all_words_across_loaded_fields() {
        let mut r = row(42, Category::Action);
        r.title = "Improve mobile settings".into();
        r.author = Some("Alice".into());
        r.labels = vec!["mobile-dev".into()];
        r.issue = Some("APP-123".into());
        assert!(matches_filter(&r, "  ALICE mobile-dev app-123 #42 "));
        assert!(matches_filter(&r, ""));
        assert!(!matches_filter(&r, "alice desktop"));
    }

    #[test]
    fn details_include_unelided_notes_and_deleted_reviewers() {
        let mut r = row(42, Category::Action);
        r.note = "merge conflict — rebase · CI failing · 3 unresolved".into();
        r.reviews = vec![prboard_core::board::ReviewSummary {
            login: None,
            state: "APPROVED".into(),
        }];
        r.stack = Some(prboard_core::board::StackInfo {
            number: 50,
            size: 3,
            position: Some(2),
            base_ref_name: "main".into(),
        });
        let detail = detail_text(&r);
        assert!(detail.contains(&r.note));
        assert!(detail.contains("deleted user — APPROVED"));
        assert!(detail.contains("Stack #50 · Layer 2 of 3 · Base: main"));
    }

    #[test]
    fn selection_resolves_by_url_across_reorder() {
        let mut d = BoardTableDelegate::new(Mode::Authored);
        d.set_rows(vec![row(1, Category::Action), row(2, Category::Action)]);
        let url2 = "https://github.com/acme/widgets/pull/2";
        let ix = d.display_index_of_url(url2).unwrap();
        assert_eq!(d.row(ix).unwrap().number, 2);
        assert!(!d.is_header(ix));

        // A higher-priority row is inserted; #2 shifts, but its URL still
        // resolves to whatever index now holds PR #2 (identity, not position).
        d.set_rows(vec![
            row(3, Category::Action),
            row(1, Category::Action),
            row(2, Category::Action),
        ]);
        let moved = d.display_index_of_url(url2).unwrap();
        assert_eq!(d.row(moved).unwrap().number, 2);

        // A vanished PR does not resolve — the caller clears selection instead
        // of pointing at an arbitrary row or a header.
        assert!(d
            .display_index_of_url("https://github.com/acme/widgets/pull/999")
            .is_none());
    }

    // ---- Responsive layout (Phase 3) + manual-resize preservation (Phase 4) ----

    fn width_of(cols: &[Column], key: &str) -> Option<f32> {
        cols.iter()
            .find(|c| c.key.as_ref() == key)
            .map(|c| f32::from(c.width))
    }

    fn total_width(cols: &[Column]) -> f32 {
        cols.iter().map(|c| f32::from(c.width)).sum()
    }

    const ALL_CLASSES: [TableWidthClass; 3] = [
        TableWidthClass::Compact,
        TableWidthClass::Medium,
        TableWidthClass::Wide,
    ];

    #[test]
    fn width_class_thresholds() {
        assert_eq!(TableWidthClass::from_width(900.0), TableWidthClass::Compact);
        assert_eq!(
            TableWidthClass::from_width(1119.0),
            TableWidthClass::Compact
        );
        assert_eq!(TableWidthClass::from_width(1120.0), TableWidthClass::Medium);
        assert_eq!(TableWidthClass::from_width(1359.0), TableWidthClass::Medium);
        assert_eq!(TableWidthClass::from_width(1360.0), TableWidthClass::Wide);
    }

    #[test]
    fn columns_fit_within_viewport_budgets() {
        for &w in &[900.0_f32, 1100.0, 1440.0, 1920.0] {
            let class = TableWidthClass::from_width(w);
            for mode in [Mode::Authored, Mode::Review] {
                let cols = columns_for(mode, class, w);
                // Column widths + scrollbar margin must stay within the viewport;
                // Note is last, so overflow would push it offscreen.
                assert!(
                    total_width(&cols) + SCROLLBAR_MARGIN <= w + 1.0,
                    "mode {mode:?} at {w}px: total {} overflows",
                    total_width(&cols)
                );
            }
        }
    }

    #[test]
    fn labels_present_except_in_compact() {
        for mode in [Mode::Authored, Mode::Review] {
            assert!(
                width_of(
                    &columns_for(mode, TableWidthClass::Compact, 1000.0),
                    "labels"
                )
                .is_none(),
                "{mode:?}: Labels should drop in Compact"
            );
            assert!(width_of(
                &columns_for(mode, TableWidthClass::Medium, 1200.0),
                "labels"
            )
            .is_some());
            assert!(
                width_of(&columns_for(mode, TableWidthClass::Wide, 1440.0), "labels").is_some()
            );
        }
    }

    #[test]
    fn required_columns_never_disappear() {
        let required = |mode: Mode| -> &'static [&'static str] {
            match mode {
                Mode::Authored => &["pr", "title", "ci", "review", "note"],
                Mode::Review => &["pr", "title", "ci", "author", "unresolved", "note"],
            }
        };
        for &class in &ALL_CLASSES {
            for mode in [Mode::Authored, Mode::Review] {
                let cols = columns_for(mode, class, 900.0);
                for key in required(mode) {
                    assert!(
                        width_of(&cols, key).is_some(),
                        "{mode:?} {class:?}: required column {key} missing"
                    );
                }
            }
        }
    }

    #[test]
    fn title_and_note_respect_minimums_and_note_priority() {
        for &w in &[900.0_f32, 1000.0, 1120.0, 1200.0, 1360.0, 1440.0, 1920.0] {
            let class = TableWidthClass::from_width(w);
            for mode in [Mode::Authored, Mode::Review] {
                let cols = columns_for(mode, class, w);
                let title = width_of(&cols, "title").unwrap();
                let note = width_of(&cols, "note").unwrap();
                assert!(
                    title >= TITLE_MIN,
                    "{mode:?} {w}px: title {title} < {TITLE_MIN}"
                );
                assert!(note >= NOTE_MIN, "{mode:?} {w}px: note {note} < {NOTE_MIN}");
                assert!(note >= title, "{mode:?} {w}px: note {note} < title {title}");
            }
        }
    }

    #[test]
    fn manual_widths_apply_only_when_count_matches() {
        let mut d = BoardTableDelegate::new(Mode::Authored);
        d.set_columns(columns_for(Mode::Authored, TableWidthClass::Wide, 1440.0));
        let n = d.column_widths().len();

        // A width vector of the wrong length (a mode-switch race) is rejected.
        assert!(!d.set_column_widths(&vec![px(50.0); n + 1]));

        // A matching vector is copied straight into the delegate columns, which
        // is exactly what TableState::refresh rebuilds col_groups from — so a
        // simulated refresh (reading column_widths back) preserves them.
        let widths: Vec<Pixels> = (0..n).map(|i| px(100.0 + i as f32)).collect();
        assert!(d.set_column_widths(&widths));
        assert_eq!(d.column_widths(), widths);
    }

    #[test]
    fn override_storage_is_bounded_and_independent() {
        use std::collections::HashMap;
        let mut overrides: HashMap<(Mode, TableWidthClass), Vec<Pixels>> = HashMap::new();
        for mode in [Mode::Authored, Mode::Review] {
            for &class in &ALL_CLASSES {
                overrides.insert((mode, class), vec![px(mode as u8 as f32), px(1.0)]);
            }
        }
        // 2 modes × 3 classes — the map can never hold more.
        assert_eq!(overrides.len(), 6);
        overrides.insert((Mode::Authored, TableWidthClass::Wide), vec![px(9.0)]);
        assert_eq!(overrides.len(), 6);
        // Each (mode, class) layout is stored independently.
        assert_ne!(
            overrides[&(Mode::Authored, TableWidthClass::Compact)],
            overrides[&(Mode::Review, TableWidthClass::Compact)]
        );
    }
}
