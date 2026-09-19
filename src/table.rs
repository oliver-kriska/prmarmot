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

use chrono::{DateTime, Local, Utc};
use gpui::prelude::FluentBuilder;
use gpui::{
    div, px, rems, AnyElement, App, ClickEvent, Context, Div, FontWeight, Hsla, InteractiveElement,
    IntoElement, MouseButton, ParentElement, Pixels, SharedString, Stateful,
    StatefulInteractiveElement, Styled, WeakEntity, Window,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::menu::{DropdownMenu, PopupMenu, PopupMenuItem};
use gpui_component::table::{Column, TableDelegate, TableState};
use gpui_component::tooltip::Tooltip;
use gpui_component::{h_flex, ActiveTheme, Sizable};
use prmarmot_core::board::{BoardRow, Ci, Mode};
use prmarmot_core::cells::{note_presentation, review_cell, NotePresentation, ReviewCell, Tone};
use prmarmot_core::layout::{layout, LayoutItem, SectionKind, Sort};
use prmarmot_core::pickup::{is_stale, wait_label, waiting_secs, DEFAULT_STALE_AFTER_DAYS};
use prmarmot_core::share::{share_group, ShareFormat, SharePayload};
use prmarmot_core::size::ChangeSize;
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use crate::design::{CHIP_HEIGHT, CHIP_PAD_X, CHIP_RADIUS, STATUS_DOT};

/// One rendered line of the table: either a category section header or a PR
/// (an index into `rows`). Headers are pseudo-rows — `row()` returns `None`
/// for them, and keyboard selection bounces off them (see `app.rs`).
enum DisplayRow {
    Header {
        label: String,
        count: Option<usize>,
        detail: Option<String>,
        /// PRs in a top-level group, in display order (indices into `rows`),
        /// kept even while the group is collapsed. Empty for stack sub-headers.
        members: Vec<usize>,
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
const PR_W: f32 = 116.0;
const CI_W: f32 = 68.0;
const UNRESOLVED_W: f32 = 52.0;
const LABELS_W: f32 = 96.0;
/// Wide windows have room for a whole chip plus its "+n" more often.
const LABELS_W_WIDE: f32 = 120.0;
const REVIEW_W_COMPACT: f32 = 160.0;
const REVIEW_W: f32 = 190.0;
const AUTHOR_W_COMPACT: f32 = 96.0;
const AUTHOR_W: f32 = 116.0;
const REPO_W_COMPACT: f32 = 120.0;
const REPO_W: f32 = 168.0;
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

/// Chip text size in px, as [`label_chip`] renders it.
const CHIP_TEXT_PX: f32 = 11.0;
/// A chip whose text room is narrower than this shows only "…"; fold it into
/// "+n" instead.
const CHIP_TEXT_MIN: f32 = 18.0;

/// Chip padding and border, both sides.
fn chip_chrome() -> f32 {
    2.0 * (CHIP_PAD_X + 1.0)
}

/// The text style a label chip's text renders with.
fn chip_text_style(window: &Window) -> gpui::TextStyle {
    let mut style = window.text_style();
    style.font_weight = FontWeight::MEDIUM;
    style
}

/// Pixel width of a label chip showing `text`.
fn chip_width(window: &mut Window, text: &str) -> f32 {
    let run = chip_text_style(window).to_run(text.len());
    let text_w = window
        .text_system()
        .layout_line(text, px(CHIP_TEXT_PX), &[run], None)
        .width;
    f32::from(text_w) + chip_chrome()
}

/// `text` shortened with "…" to fit a chip's text room of `max_width`.
fn elide_chip_text(window: &mut Window, text: &str, max_width: Pixels) -> String {
    let style = chip_text_style(window);
    let runs = vec![style.to_run(text.len())];
    window
        .text_system()
        .line_wrapper(style.font(), px(CHIP_TEXT_PX))
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

/// How many whole chips of `widths` fit in `avail`, in order, `gap` apart,
/// keeping room for the "+n" chip (`more(n)` wide) while any are left out.
fn chips_that_fit(widths: &[f32], more: impl Fn(usize) -> f32, avail: f32, gap: f32) -> usize {
    let mut used = 0.0;
    let mut shown = 0;
    for (ix, width) in widths.iter().enumerate() {
        let next = used + if ix == 0 { 0.0 } else { gap } + width;
        let left_out = widths.len() - ix - 1;
        let reserve = if left_out == 0 {
            0.0
        } else {
            gap + more(left_out)
        };
        if next + reserve > avail {
            break;
        }
        used = next;
        shown = ix + 1;
    }
    shown
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
/// Human scanning order (critique #3): identity first (PR), then
/// WHAT it is (Title), then health (CI), then the merged Review / Author +
/// Unresolved metadata, then Labels, then the Note. Fixed metadata columns get
/// bounded widths; Title and Note split the remaining viewport with Note kept
/// at least as wide as Title. Labels drop out in Compact. Pure — no GPUI
/// context — so the layout is unit-tested directly.
pub fn columns_for(
    mode: Mode,
    class: TableWidthClass,
    viewport_width: f32,
    all_repos: bool,
) -> Vec<Column> {
    let compact = class == TableWidthClass::Compact;
    let show_labels = !compact;
    let review_w = if compact { REVIEW_W_COMPACT } else { REVIEW_W };
    let author_w = if compact { AUTHOR_W_COMPACT } else { AUTHOR_W };
    let labels_w = match class {
        TableWidthClass::Compact => 0.0,
        TableWidthClass::Medium => LABELS_W,
        TableWidthClass::Wide => LABELS_W_WIDE,
    };
    let repo_w = if all_repos {
        if compact {
            REPO_W_COMPACT
        } else {
            REPO_W
        }
    } else {
        0.0
    };

    let fixed_sum = match mode {
        Mode::Authored => PR_W + CI_W + review_w + labels_w + repo_w,
        Mode::Review => PR_W + CI_W + author_w + UNRESOLVED_W + labels_w + repo_w,
    };
    // Elastic remainder, floored so Title/Note always meet their minimums.
    let title_min = if all_repos && compact {
        // Reserve room for the always-visible watch control at the 900px floor.
        168.0
    } else {
        TITLE_MIN
    };
    let note_min = if all_repos && compact {
        224.0
    } else {
        NOTE_MIN
    };
    let flexible = (viewport_width - fixed_sum - SCROLLBAR_MARGIN).max(title_min + note_min);
    let mut title_w = ((flexible * TITLE_FLEX_RATIO / QUANTUM).round() * QUANTUM).max(title_min);
    let note_w = (flexible - title_w).max(note_min);
    if note_w < title_w {
        // Never let Title out-grow Note — Note is the product.
        title_w = note_w;
    }

    let col = |key: &'static str, name: &'static str, w: f32| Column::new(key, name).width(px(w));
    match mode {
        Mode::Authored => {
            let mut cols = vec![col("pr", "PR", PR_W)];
            if all_repos {
                cols.push(col("repo", "Repo", repo_w));
            }
            cols.extend([
                col("title", "Title", title_w),
                col("ci", "CI", CI_W),
                col("review", "Review", review_w),
            ]);
            if show_labels {
                cols.push(col("labels", "Labels", labels_w));
            }
            cols.push(col("note", "Note", note_w));
            cols
        }
        Mode::Review => {
            let mut cols = vec![col("pr", "PR", PR_W)];
            if all_repos {
                cols.push(col("repo", "Repo", repo_w));
            }
            cols.extend([
                col("title", "Title", title_w),
                col("ci", "CI", CI_W),
                col("author", "Author", author_w),
                col("unresolved", "Unres", UNRESOLVED_W),
            ]);
            if show_labels {
                cols.push(col("labels", "Labels", labels_w));
            }
            cols.push(col("note", "Note", note_w));
            cols
        }
    }
}

// The search grammar lives in `prmarmot-core` so the desktop app and the iPad
// cannot drift on what `label:"help wanted"` means. Only the drawing stays
// here.
pub use prmarmot_core::search::{
    matches_filter, take_filter_chips, with_filter, FilterChip, Qualifier, StaleRule,
};

/// One neutral chip style for labels (spec §7): GitHub's arbitrary label
/// hues would out-shout the status system. Also used for the search tokens.
pub fn label_chip(theme: &gpui_component::Theme) -> Div {
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
}

/// "Small · 42 changed lines in 3 files (+30 −12)".
/// The local time zone, as an offset in seconds, for the one line of the
/// panel that shows a wall clock.
pub(crate) fn local_offset_secs() -> i32 {
    Local::now().offset().local_minus_utc()
}

fn size_text(size: ChangeSize) -> String {
    prmarmot_core::detail::size_text(size)
}

/// Full, unelided snapshot details; no secondary network request or hidden
/// cache. The lines themselves are core's, so the iPad shows the same ones.
pub fn detail_text(row: &BoardRow, mode: Mode) -> String {
    detail_text_at(row, mode, Utc::now())
}

/// [`detail_text`] with the wait measured at `now`.
fn detail_text_at(row: &BoardRow, mode: Mode, now: DateTime<Utc>) -> String {
    prmarmot_core::detail::detail_text(row, mode, now, local_offset_secs())
}

#[derive(Clone)]
pub enum RowAction {
    Open,
    Copy(String),
    Details,
    Watch,
    Snooze,
    CancelSnooze,
}

pub fn row_copy_items(row: &BoardRow, mode: Mode) -> Vec<(&'static str, String)> {
    prmarmot_core::detail::copy_items(row, mode, Utc::now(), local_offset_secs())
}

type RowActionHandler = std::rc::Rc<dyn Fn(BoardRow, RowAction, &mut Window, &mut App)>;
type FilterClickHandler = Rc<dyn Fn(FilterChip, &mut Window, &mut App)>;
type GroupCopyHandler = Rc<dyn Fn(GroupCopy, &mut Window, &mut App)>;

/// A board group rendered for the clipboard, ready for `app.rs` to write.
pub struct GroupCopy {
    pub format: ShareFormat,
    pub count: usize,
    pub payload: SharePayload,
}

/// Group copy formats, richest first, labelled with where each pastes best.
const GROUP_COPY_ITEMS: [(&str, ShareFormat); 4] = [
    ("Copy list for Slack & docs", ShareFormat::List),
    ("Copy Markdown for GitHub", ShareFormat::Markdown),
    ("Copy as table", ShareFormat::Table),
    ("Copy URLs", ShareFormat::Urls),
];

/// Hover group for a section header's reveal-on-hover Copy button.
const HEADER_GROUP: &str = "board-section-header";

pub struct BoardTableDelegate {
    rows: Vec<BoardRow>,
    display: Vec<DisplayRow>,
    columns: Vec<Column>,
    mode: Mode,
    all_repos: bool,
    /// Changed-marker tooltip text by PR id; present only for changed PRs.
    changed: HashMap<String, SharedString>,
    watched: HashSet<String>,
    snoozed: HashSet<String>,
    show_snoozed: bool,
    /// A wait this many days long shows as stale.
    stale_after_days: u64,
    /// The order inside the review queue's pickup sections.
    sort: Sort,
    pub on_row_action: Option<RowActionHandler>,
    pub on_group_copy: Option<GroupCopyHandler>,
    /// A label, author, or repository was clicked: add it to the search.
    pub on_filter_click: Option<FilterClickHandler>,
    /// Display index of the header whose Copy menu is open, so its button
    /// stays visible while the pointer is over the menu instead of the header.
    copy_menu_open: Rc<Cell<Option<usize>>>,
}

impl BoardTableDelegate {
    pub fn new(mode: Mode, all_repos: bool) -> Self {
        Self {
            rows: Vec::new(),
            display: Vec::new(),
            columns: columns_for(
                mode,
                TableWidthClass::Medium,
                DEFAULT_VIEWPORT_WIDTH,
                all_repos,
            ),
            mode,
            all_repos,
            changed: HashMap::new(),
            watched: HashSet::new(),
            snoozed: HashSet::new(),
            show_snoozed: false,
            stale_after_days: DEFAULT_STALE_AFTER_DAYS,
            sort: Sort::Wait,
            on_row_action: None,
            on_group_copy: None,
            on_filter_click: None,
            copy_menu_open: Rc::new(Cell::new(None)),
        }
    }

    /// Switch queue: change the sort/grouping mode and re-band the display.
    /// Columns are owned by `RootView` (they depend on the live window width),
    /// so it calls `set_columns` right after this.
    pub fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
        self.rebuild_display();
    }

    pub fn set_scope(&mut self, all_repos: bool) {
        self.all_repos = all_repos;
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

    pub fn set_stale_after_days(&mut self, days: u64) {
        self.stale_after_days = days;
    }

    /// Takes effect with the next `set_rows`.
    pub fn set_sort(&mut self, sort: Sort) {
        self.sort = sort;
    }

    pub fn set_rows(&mut self, rows: Vec<BoardRow>) {
        self.rows = rows;
        self.rebuild_display();
    }

    pub fn set_attention(
        &mut self,
        changed: HashMap<String, SharedString>,
        watched: HashSet<String>,
        snoozed: HashSet<String>,
        show_snoozed: bool,
    ) {
        self.changed = changed;
        self.watched = watched;
        self.snoozed = snoozed;
        self.show_snoozed = show_snoozed;
        self.rebuild_display();
    }

    /// Rebuild display rows from the shared core layout (sections, the Approved
    /// split, stacks, Snoozed), adding the window's own Snoozed wording.
    fn rebuild_display(&mut self) {
        let show_snoozed = self.show_snoozed;
        self.display = layout(
            &self.rows,
            self.mode,
            self.all_repos,
            &self.snoozed,
            show_snoozed,
            self.sort,
        )
        .into_iter()
        .map(|item| match item {
            LayoutItem::Row(ix) => DisplayRow::Pr(ix),
            LayoutItem::Header {
                kind,
                label,
                count,
                detail,
                members,
            } => DisplayRow::Header {
                label,
                count,
                detail: if kind == SectionKind::Snoozed {
                    Some(if show_snoozed {
                        "shown · use Snoozed to collapse".into()
                    } else {
                        "collapsed · use Snoozed to show".into()
                    })
                } else {
                    detail
                },
                members,
            },
        })
        .collect();
    }

    /// End the tree at the visible section boundary, even if other layers
    /// exist elsewhere. Standalone rows must never look like stack children.
    fn stack_ends_at(&self, display_ix: usize) -> bool {
        let Some(row) = self.row(display_ix) else {
            return false;
        };
        let Some(stack) = &row.stack else {
            return false;
        };
        self.row(display_ix + 1).is_none_or(|next| {
            next.repo != row.repo
                || next
                    .stack
                    .as_ref()
                    .is_none_or(|next_stack| next_stack.number != stack.number)
        })
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

    /// The top-level group label a display row belongs to (a header is its
    /// own group; stack sub-headers belong to the group above them).
    pub fn group_label_at(&self, display_ix: usize) -> Option<String> {
        self.display
            .get(..=display_ix)?
            .iter()
            .rev()
            .find_map(|d| match d {
                DisplayRow::Header {
                    label,
                    count: Some(_),
                    ..
                } => Some(label.clone()),
                _ => None,
            })
    }

    /// Render the group named `label` for the clipboard. Groups are looked up
    /// by label, which is unique per board, so a menu opened before a refresh
    /// copies the group's current rows rather than whatever moved to its index.
    pub fn group_copy(&self, label: &str, format: ShareFormat) -> Option<GroupCopy> {
        let members = self.display.iter().find_map(|d| match d {
            DisplayRow::Header {
                label: header,
                count: Some(_),
                members,
                ..
            } if header == label => Some(members),
            _ => None,
        })?;
        let rows: Vec<BoardRow> = members
            .iter()
            .filter_map(|&ix| self.rows.get(ix).cloned())
            .collect();
        if rows.is_empty() {
            return None;
        }
        Some(GroupCopy {
            format,
            count: rows.len(),
            payload: share_group(label, &rows, self.mode, format),
        })
    }
}

/// Group copy items for a header's context menu or Copy dropdown. Rows are
/// read when an item is clicked, not when the menu is built.
fn group_copy_menu(
    mut menu: PopupMenu,
    table: WeakEntity<TableState<BoardTableDelegate>>,
    label: String,
) -> PopupMenu {
    for (item_label, format) in GROUP_COPY_ITEMS {
        let table = table.clone();
        let label = label.clone();
        menu = menu.item(
            PopupMenuItem::new(item_label).on_click(move |_, window, cx| {
                let Some(table) = table.upgrade() else {
                    return;
                };
                let (copy, handler) = {
                    let delegate = table.read(cx).delegate();
                    (
                        delegate.group_copy(&label, format),
                        delegate.on_group_copy.clone(),
                    )
                };
                if let (Some(copy), Some(handler)) = (copy, handler) {
                    handler(copy, window, cx);
                }
            }),
        );
    }
    menu
}

impl BoardTableDelegate {
    /// Cell text that adds `chip` to the search when clicked, if the board
    /// handles filter clicks. The click never reaches the row.
    fn filter_target(
        &self,
        id: (&'static str, usize),
        text: String,
        chip: Option<FilterChip>,
    ) -> AnyElement {
        let (Some(handler), Some(chip)) = (self.on_filter_click.clone(), chip) else {
            return div().child(text).into_any_element();
        };
        let tip = format!("Filter by {}", chip.term());
        h_flex()
            .min_w_0()
            .child(
                div()
                    .id(id)
                    .cursor_pointer()
                    .hover(|style| style.underline())
                    .child(text)
                    .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(move |event: &ClickEvent, window, cx| {
                        cx.stop_propagation();
                        if event.click_count() == 1 {
                            handler(chip.clone(), window, cx);
                        }
                    }),
            )
            .into_any_element()
    }
}

/// Tooltip for the blue changed marker: why it is shown, what changed, and
/// how to clear it.
pub fn changed_marker_tooltip(changes: &[String]) -> SharedString {
    prmarmot_core::status::changed_marker_tooltip(changes).into()
}

/// The theme colour for one of core's tones. Core decides *which* tone a cell
/// has; each front end decides what that looks like.
fn tone_color(tone: Tone, theme: &gpui_component::theme::Theme) -> Hsla {
    match tone {
        Tone::Danger => theme.danger,
        Tone::Warning => theme.warning,
        Tone::Success => theme.success,
        Tone::Routine | Tone::Muted => theme.muted_foreground,
    }
}

fn status_dot(color: Hsla) -> Div {
    div()
        .size(px(STATUS_DOT))
        .rounded_full()
        .flex_shrink_0()
        .bg(color)
}

impl TableDelegate for BoardTableDelegate {
    fn context_menu(
        &mut self,
        row_ix: usize,
        mut menu: PopupMenu,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> PopupMenu {
        if let Some(DisplayRow::Header {
            label,
            count: Some(_),
            members,
            ..
        }) = self.display.get(row_ix)
        {
            if members.is_empty() {
                return menu;
            }
            return group_copy_menu(menu, cx.entity().downgrade(), label.clone());
        }
        let Some(row) = self.row(row_ix).cloned() else {
            return menu;
        };
        let Some(handler) = self.on_row_action.clone() else {
            return menu;
        };
        let mut actions = vec![
            ("Open on GitHub", RowAction::Open),
            ("Show details", RowAction::Details),
        ];
        actions.extend(
            row_copy_items(&row, self.mode)
                .into_iter()
                .map(|(label, value)| (label, RowAction::Copy(value))),
        );
        actions.push((
            if self.watched.contains(&row.id) {
                "Unwatch PR"
            } else {
                "Watch PR"
            },
            RowAction::Watch,
        ));
        actions.push(("Snooze…", RowAction::Snooze));
        if self.snoozed.contains(&row.id) {
            actions.push(("Cancel snooze", RowAction::CancelSnooze));
        }
        for (index, (label, action)) in actions.into_iter().enumerate() {
            if index == 2 || index == 7 {
                menu = menu.separator();
            }
            let row = row.clone();
            let handler = handler.clone();
            menu = menu.item(PopupMenuItem::new(label).on_click(move |_, window, cx| {
                handler(row.clone(), action.clone(), window, cx);
            }));
        }
        menu
    }

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
                members,
            }) => tr
                .relative()
                .group(HEADER_GROUP)
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
                        })
                        // Revealed on header hover (a style swap, no animation)
                        // and held while its menu is open.
                        .when(count.is_some() && !members.is_empty(), |header| {
                            let open_state = self.copy_menu_open.clone();
                            let table = cx.entity().downgrade();
                            let label = label.clone();
                            header.child(
                                div()
                                    .when(open_state.get() != Some(row_ix), |slot| {
                                        slot.invisible()
                                            .group_hover(HEADER_GROUP, |style| style.visible())
                                    })
                                    .child(
                                        Button::new(("copy-group", row_ix))
                                            .ghost()
                                            .xsmall()
                                            .label("Copy")
                                            .dropdown_caret(true)
                                            .dropdown_menu(move |menu, _, _| {
                                                group_copy_menu(menu, table.clone(), label.clone())
                                            })
                                            .on_open_change(move |open, window, _| {
                                                open_state.set(open.then_some(row_ix));
                                                window.refresh();
                                            }),
                                    ),
                            )
                        }),
                ),
            // No per-row tint: the "Needs action" section header, the red note
            // dot, and the red problem phrase already signal urgency three
            // times over. A red row band on top of that dominates the screen
            // and flattens individual CI-fail / conflict rows into one alarm
            // block (design review, 2026-07-24). Zebra striping stays; state
            // lives in the Note cell.
            Some(DisplayRow::Pr(ix)) => tr
                // Built-in striping generates filler rows below the data.
                // Style real PRs here instead; headers keep their own bands.
                .when(ix % 2 != 0, |row| row.bg(theme.table_even))
                .when(self.stack_ends_at(row_ix), |row| {
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
                // PR. Drafts need no badge here: they have their own section,
                // a dimmed number, and a Note that starts with "draft".
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
                let mut cell = h_flex()
                    .relative()
                    .w_full()
                    .h(px(20.))
                    .pr(px(24.))
                    .gap_1()
                    .items_center();
                if let Some(tooltip) = self.changed.get(&row.id).cloned() {
                    // A padded hover target (offset by an equal negative
                    // margin) so the 7px dot is easy to point at without
                    // shifting the PR number.
                    cell = cell.child(
                        div()
                            .id(("changed-marker", row_ix))
                            .flex_shrink_0()
                            .p(px(4.))
                            .m(px(-4.))
                            .child(status_dot(theme.link))
                            .tooltip(move |window, cx| {
                                Tooltip::new(tooltip.clone()).build(window, cx)
                            }),
                    );
                }
                cell = cell.child(number);
                if let Some(handler) = self.on_row_action.clone() {
                    let watched = self.watched.contains(&row.id);
                    let watched_row = row.clone();
                    cell = cell.child(
                        div()
                            .id(("watch-pr", row_ix))
                            .absolute()
                            .right_0()
                            .top_0()
                            .flex_shrink_0()
                            .size(px(20.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .text_color(if watched {
                                theme.foreground
                            } else {
                                theme.muted_foreground.opacity(0.55)
                            })
                            .hover(|style| style.bg(theme.muted).text_color(theme.foreground))
                            .child(
                                gpui::svg()
                                    .path("icons/binoculars.svg")
                                    .size(px(15.))
                                    .text_color(if watched {
                                        theme.foreground
                                    } else {
                                        theme.muted_foreground.opacity(0.55)
                                    }),
                            )
                            .tooltip(move |window, cx| {
                                Tooltip::new(if watched {
                                    "Unwatch PR · w"
                                } else {
                                    "Watch PR · w"
                                })
                                .build(window, cx)
                            })
                            .on_mouse_down(MouseButton::Left, move |event, window, cx| {
                                cx.stop_propagation();
                                if event.click_count == 1 {
                                    handler(watched_row.clone(), RowAction::Watch, window, cx);
                                }
                            })
                            .on_click(|_, _, cx| cx.stop_propagation()),
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
                    // Chips; "bug" is the loud one. Only whole chips are
                    // drawn — each is a click target — and the rest fold into
                    // "+n". The tooltip carries the full list.
                    let full = row.labels.join(", ");
                    // "bug" must never hide behind the +n overflow.
                    let mut ordered: Vec<String> = row.labels.clone();
                    ordered.sort_by_key(|l| l != "bug");
                    // 🐛 is the single permitted emoji: semantic, not decorative.
                    let texts: Vec<String> = ordered
                        .iter()
                        .map(|label| {
                            if label == "bug" {
                                format!("🐛 {label}")
                            } else {
                                label.clone()
                            }
                        })
                        .collect();
                    let avail = f32::from(self.columns[col_ix].width) - CELL_PAD_X - ELIDE_SAFETY;
                    let widths: Vec<f32> = texts.iter().map(|t| chip_width(window, t)).collect();
                    let more: Vec<f32> = (0..=texts.len())
                        .map(|n| chip_width(window, &format!("+{n}")))
                        .collect();
                    let shown = chips_that_fit(&widths, |n| more[n], avail, GAP_1);
                    let mut visible: Vec<(usize, String)> =
                        texts.iter().cloned().enumerate().take(shown).collect();
                    if shown == 0 {
                        // Not even one whole chip: shorten the first one.
                        let left_out = texts.len() - 1;
                        let reserve = if left_out == 0 {
                            0.0
                        } else {
                            GAP_1 + more[left_out]
                        };
                        let room = avail - reserve - chip_chrome();
                        if room >= CHIP_TEXT_MIN {
                            visible.push((0, elide_chip_text(window, &texts[0], px(room))));
                        }
                    }
                    let hidden = texts.len() - visible.len();
                    let mut chips = h_flex().gap_1().overflow_hidden();
                    let (hover_border, hover_text) = (muted, theme.foreground);
                    for (ix, text) in visible {
                        let label = &ordered[ix];
                        let chip = label_chip(theme)
                            .flex_shrink_0()
                            .text_color(theme.secondary_foreground)
                            .child(text);
                        // A click filters the board by the label. It must not
                        // reach the row, which would select it or open details.
                        chips = chips.child(match self.on_filter_click.clone() {
                            Some(handler) => {
                                let filter = FilterChip::new(Qualifier::Label, label.as_str());
                                chip.id(("label-chip", ix))
                                    .cursor_pointer()
                                    .hover(|style| {
                                        style.border_color(hover_border).text_color(hover_text)
                                    })
                                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                        cx.stop_propagation()
                                    })
                                    .on_click(move |event: &ClickEvent, window, cx| {
                                        cx.stop_propagation();
                                        if event.click_count() == 1 {
                                            handler(filter.clone(), window, cx);
                                        }
                                    })
                                    .into_any_element()
                            }
                            None => chip.into_any_element(),
                        });
                    }
                    if hidden > 0 {
                        let more = format!("+{hidden}");
                        chips = chips.child(match self.on_filter_click.clone() {
                            // "+n" opens the hidden labels, each a filter.
                            Some(handler) => {
                                let rest = ordered[ordered.len() - hidden..].to_vec();
                                div()
                                    .flex_shrink_0()
                                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                        cx.stop_propagation()
                                    })
                                    .child(
                                        Button::new(("more-labels", row_ix))
                                            .ghost()
                                            .xsmall()
                                            .h(px(CHIP_HEIGHT))
                                            .px(px(CHIP_PAD_X))
                                            .rounded(px(CHIP_RADIUS))
                                            .bg(theme.muted)
                                            .border_1()
                                            .border_color(theme.border)
                                            .child(
                                                // A Button label ignores the
                                                // button's text size.
                                                div()
                                                    .text_size(px(CHIP_TEXT_PX))
                                                    .font_weight(FontWeight::MEDIUM)
                                                    .text_color(muted)
                                                    .child(more),
                                            )
                                            .dropdown_menu(move |mut menu, _, _| {
                                                menu = menu
                                                    .label("Filter by label")
                                                    .scrollable(true)
                                                    .max_h(px(320.));
                                                for label in &rest {
                                                    let handler = handler.clone();
                                                    let target =
                                                        FilterChip::new(Qualifier::Label, label);
                                                    menu = menu.item(
                                                        PopupMenuItem::new(label.clone()).on_click(
                                                            move |_, window, cx| {
                                                                handler(target.clone(), window, cx)
                                                            },
                                                        ),
                                                    );
                                                }
                                                menu
                                            }),
                                    )
                                    .into_any_element()
                            }
                            None => label_chip(theme)
                                .flex_shrink_0()
                                .text_color(muted)
                                .child(more)
                                .into_any_element(),
                        });
                    }
                    let tip: SharedString = if self.on_filter_click.is_some() {
                        format!("{full}\nClick a label to search for label:<name>").into()
                    } else {
                        full.into()
                    };
                    return chips
                        .id(("labels", row_ix))
                        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
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
                // The token may not read the checks: a word, never the dash
                // that means "no checks".
                Ci::Hidden => h_flex().child(div().text_color(muted).child("hidden")),
            },
            "repo" => {
                let chip = FilterChip::new(Qualifier::Repo, row.repo.as_str());
                return div()
                    .text_color(muted)
                    .child(self.filter_target(
                        ("repo-filter", row_ix),
                        row.repo.clone(),
                        Some(chip),
                    ))
                    .into_any_element();
            }
            "author" => {
                let chip = row
                    .author
                    .as_deref()
                    .map(|author| FilterChip::new(Qualifier::Author, author));
                let text = row.author.clone().unwrap_or_else(|| "?".into());
                return self
                    .filter_target(("author-filter", row_ix), text, chip)
                    .into_any_element();
            }
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
                // "Not requested". Most cells were empty split across two
                // columns — this recovers ~150px for Title/Note. The wording,
                // the glyphs and the order are core's, so the iPad says the
                // same; only the colours are the theme's.
                match review_cell(row) {
                    ReviewCell::Reviewed {
                        marks,
                        summary,
                        hover,
                    } => {
                        let mut cell = h_flex().gap_2().items_center().overflow_hidden();
                        for mark in marks {
                            cell = cell.child(
                                h_flex()
                                    .gap_1()
                                    .items_center()
                                    .whitespace_nowrap()
                                    .child(
                                        div()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(tone_color(mark.tone, theme))
                                            .child(mark.glyph),
                                    )
                                    .child(mark.login),
                            );
                        }
                        cell = cell.child(div().flex_shrink_0().text_color(muted).child(summary));
                        return cell
                            .id(("review", row_ix))
                            .tooltip(move |window, cx| {
                                Tooltip::new(hover.clone()).build(window, cx)
                            })
                            .into_any_element();
                    }
                    ReviewCell::Requested {
                        names,
                        arrow,
                        suffix,
                        hover,
                    } => {
                        let full = hover;
                        // Elide the names to the room left by the "→" and the
                        // "— requested" suffix (both flex_shrink_0), with a real "…".
                        let arrow_w = measure_width(window, &arrow);
                        let suffix_w = measure_width(window, &suffix);
                        let col_w = self.columns[col_ix].width;
                        let avail = col_w
                            - arrow_w
                            - suffix_w
                            - px(CELL_PAD_X + GAP_1 + GAP_1 + ELIDE_SAFETY);
                        let names = elide(window, &names, avail);
                        return h_flex()
                            .w_full()
                            .gap_1()
                            .items_center()
                            .overflow_hidden()
                            .child(div().flex_shrink_0().text_color(muted).child(arrow))
                            .child(div().flex_shrink_0().child(names))
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .text_color(muted.opacity(0.7))
                                    .child(suffix),
                            )
                            .id(("review", row_ix))
                            .tooltip(move |window, cx| Tooltip::new(full.clone()).build(window, cx))
                            .into_any_element();
                    }
                    ReviewCell::NotRequested { text } => div().text_color(muted).child(text),
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
                    mut tooltip,
                } = note_presentation(row);
                // How long it has waited for a reviewer trails the Note and,
                // like the primary, never elides; a stale wait is amber.
                let now = Utc::now();
                let wait = waiting_secs(row, now).map(|secs| {
                    let stale = is_stale(row, now, self.stale_after_days);
                    (format!(" · {}", wait_label(secs)), stale)
                });
                if let Some((label, stale)) = &wait {
                    tooltip.push_str(&format!(
                        "\nWaiting for a reviewer:{}{}",
                        label.trim_start_matches(" ·"),
                        if *stale { " (stale)" } else { "" }
                    ));
                }
                // In the review queue the size band trails last, muted.
                let size = row.size.filter(|_| self.mode == Mode::Review);
                if let Some(size) = size {
                    tooltip.push_str(&format!("\nSize: {}", size_text(size)));
                }
                let size = size.map(|size| format!(" · {}", size.band().label()));
                let (dot_color, primary_color) = match tone {
                    Tone::Danger => (Some(theme.danger), theme.danger),
                    Tone::Warning => (Some(theme.warning), muted),
                    Tone::Success => (Some(theme.success), muted),
                    Tone::Routine => (Some(muted), muted),
                    Tone::Muted => (None, muted),
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
                let wait_w = wait
                    .as_ref()
                    .map_or(px(0.), |(label, _)| measure_width(window, label));
                let size_w = size
                    .as_ref()
                    .map_or(px(0.), |label| measure_width(window, label));
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
                        - wait_w
                        - size_w
                        - px(CELL_PAD_X + 8.0 + dot_region + GAP_1P5 * 3. + ELIDE_SAFETY);
                    let tail = elide(window, &tail, avail);
                    cell = cell.child(div().flex_shrink_0().text_color(muted).child(tail));
                }
                if let Some((label, stale)) = wait {
                    cell = cell.child(
                        div()
                            .flex_shrink_0()
                            .text_color(if stale { theme.warning } else { muted })
                            .child(label),
                    );
                }
                if let Some(label) = size {
                    cell = cell.child(div().flex_shrink_0().text_color(muted).child(label));
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
        let msg = prmarmot_core::status::queue_empty_text(self.mode, self.all_repos);
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
    use prmarmot_core::board::{Blocker, Category, ReviewState};
    use prmarmot_core::layout::group_label;

    fn rule() -> StaleRule {
        StaleRule {
            now: DateTime::parse_from_rfc3339("2026-09-15T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            after_days: 3,
        }
    }

    /// The search at a fixed time, with the default stale rule.
    fn filtered(row: &BoardRow, query: &str) -> bool {
        matches_filter(row, query, rule())
    }

    #[test]
    fn copied_reference_distinguishes_same_number_in_different_repositories() {
        let first = row(418, Category::Action);
        let mut second = first.clone();
        second.repo = "other/mobile".into();
        second.url = "https://github.com/other/mobile/pull/418".into();
        let copies = row_copy_items(&second, Mode::Authored);
        assert_eq!(copies[0].1, "https://github.com/other/mobile/pull/418");
        assert_eq!(copies[1].1, "#418");
        assert_eq!(copies[2].1, "other/mobile#418");
        assert_ne!(row_copy_items(&first, Mode::Authored)[2].1, copies[2].1);
        assert!(copies[4].1.starts_with("other/mobile#418 "));
    }

    fn row(number: u64, category: Category) -> BoardRow {
        BoardRow {
            id: format!("https://github.com/acme/widgets/pull/{number}"),
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
            author: None,
            stack: None,
            queue_provenance: None,
            draft: matches!(category, Category::Draft),
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
        assert_eq!(p.tone, Tone::Warning);
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
        assert_eq!(p.tone, Tone::Warning);
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
        assert_eq!(p.tone, Tone::Danger);
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
        assert_eq!(p.tone, Tone::Danger);
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
        assert_eq!(p.tone, Tone::Danger);
        assert_eq!(p.primary, "CI failing");
        assert_eq!(p.context, vec!["2 unresolved".to_string()]);
    }

    #[test]
    fn routine_review_queue_item_is_warning_not_danger() {
        let mut r = row(5, Category::Todo);
        r.note = "🔵 needs your review".into();
        let p = note_presentation(&r);
        assert_eq!(p.tone, Tone::Warning);
        assert_eq!(p.primary, "needs your review");
    }

    #[test]
    fn review_queue_with_bad_ci_or_conflict_is_danger() {
        let mut r = row(6, Category::Todo);
        r.ci = Ci::Fail;
        r.note = "⚠️ CI red — maybe wait for green".into();
        let p = note_presentation(&r);
        assert_eq!(p.tone, Tone::Danger);
        assert_eq!(p.primary, "CI red");
        assert_eq!(p.remedy.as_deref(), Some("maybe wait for green"));

        let mut r2 = row(7, Category::Todo);
        r2.conflict = true;
        r2.note = "⚠️ has conflicts".into();
        assert_eq!(note_presentation(&r2).tone, Tone::Danger);
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
    fn stacks_with_the_same_number_in_different_repositories_stay_separate() {
        let mut a = row(42, Category::Action);
        a.stack = Some(prmarmot_core::board::StackInfo {
            number: 7,
            size: 1,
            base_ref_name: "main".into(),
            position: Some(1),
        });
        let mut b = a.clone();
        b.repo = "other/widgets".into();
        b.url = "https://github.com/other/widgets/pull/42".into();
        b.id = b.url.clone();
        let mut d = BoardTableDelegate::new(Mode::Authored, false);
        d.set_rows(vec![a, b]);
        let identities: Vec<_> = (0..d.display_len())
            .filter_map(|i| d.row(i))
            .map(|r| r.repo.as_str())
            .collect();
        assert_eq!(identities, ["acme/widgets", "other/widgets"]);
        assert_eq!(d.display_len(), 5); // category + two stack headers + two PRs
        assert!(d.stack_ends_at(2));
        assert!(d.stack_ends_at(4));
        assert_eq!(
            d.display_index_of_url("https://github.com/other/widgets/pull/42"),
            Some(4)
        );

        d.set_scope(true);
        assert!(matches!(
            &d.display[1],
            DisplayRow::Header { label, .. } if label == "acme/widgets · Stack #7"
        ));
        assert!(matches!(
            &d.display[3],
            DisplayRow::Header { label, .. } if label == "other/widgets · Stack #7"
        ));
    }

    #[test]
    fn headers_group_contiguous_categories_with_counts() {
        let mut d = BoardTableDelegate::new(Mode::Authored, false);
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
    fn snoozed_rows_are_collapsed_until_explicitly_shown() {
        let mut delegate = BoardTableDelegate::new(Mode::Authored, false);
        delegate.set_rows(vec![row(1, Category::Action), row(2, Category::Await)]);
        delegate.set_attention(
            HashMap::new(),
            HashSet::new(),
            HashSet::from(["https://github.com/acme/widgets/pull/1".into()]),
            false,
        );
        assert!(delegate
            .display_index_of_url("https://github.com/acme/widgets/pull/1")
            .is_none());
        assert!(delegate
            .display_index_of_url("https://github.com/acme/widgets/pull/2")
            .is_some());
        delegate.show_snoozed = true;
        delegate.rebuild_display();
        assert!(delegate
            .display_index_of_url("https://github.com/acme/widgets/pull/1")
            .is_some());
    }

    #[test]
    fn changed_marker_tooltip_says_what_changed_and_how_to_clear() {
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
    fn group_copy_takes_display_order_including_stack_layers() {
        let mut base = row(2, Category::Action);
        base.stack = Some(prmarmot_core::board::StackInfo {
            number: 70,
            size: 2,
            base_ref_name: "main".into(),
            position: Some(1),
        });
        let mut top = row(4, Category::Action);
        top.review_state = ReviewState::Approved;
        top.stack = Some(prmarmot_core::board::StackInfo {
            position: Some(2),
            ..base.stack.clone().unwrap()
        });
        let mut d = BoardTableDelegate::new(Mode::Authored, false);
        d.set_rows(vec![
            row(1, Category::Action),
            base,
            row(3, Category::Action),
            top,
            row(5, Category::Await),
        ]);
        let copy = d.group_copy("Needs action", ShareFormat::Urls).unwrap();
        assert_eq!(copy.count, 4);
        assert_eq!(
            copy.payload.plain,
            [2, 4, 1, 3]
                .map(|n| format!("https://github.com/acme/widgets/pull/{n}"))
                .join("\n")
        );
        // A stack layer and the stack sub-header both belong to "Needs action".
        assert_eq!(d.group_label_at(1).as_deref(), Some("Needs action"));
        assert_eq!(d.group_label_at(2).as_deref(), Some("Needs action"));
        let await_ix = d
            .display_index_of_url("https://github.com/acme/widgets/pull/5")
            .unwrap();
        assert_eq!(
            d.group_label_at(await_ix).as_deref(),
            Some("Awaiting review")
        );
        assert!(d.group_copy("Stack #70", ShareFormat::Urls).is_none());
        assert!(d.group_copy("Drafts", ShareFormat::Urls).is_none());
    }

    #[test]
    fn collapsed_snoozed_group_still_copies_its_rows() {
        let mut d = BoardTableDelegate::new(Mode::Authored, false);
        d.set_rows(vec![row(1, Category::Action), row(2, Category::Await)]);
        d.set_attention(
            HashMap::new(),
            HashSet::new(),
            HashSet::from(["https://github.com/acme/widgets/pull/1".into()]),
            false,
        );
        let copy = d.group_copy("Snoozed", ShareFormat::Markdown).unwrap();
        assert_eq!(copy.count, 1);
        assert!(copy
            .payload
            .plain
            .contains("(https://github.com/acme/widgets/pull/1)"));
        assert_eq!(
            d.group_copy("Awaiting review", ShareFormat::Urls)
                .unwrap()
                .payload
                .plain,
            "https://github.com/acme/widgets/pull/2"
        );
    }

    #[test]
    fn approved_section_separates_interleaved_approvals_but_keeps_blockers_and_drafts() {
        let approved = |number, category| {
            let mut r = row(number, category);
            r.review_state = ReviewState::Approved;
            r
        };
        let mut d = BoardTableDelegate::new(Mode::Authored, false);
        d.set_rows(vec![
            approved(10, Category::Action),
            row(20, Category::Await),
            approved(30, Category::Await),
            row(40, Category::Await),
            approved(50, Category::Await),
            approved(60, Category::Draft),
        ]);
        let headers: Vec<_> = d
            .display
            .iter()
            .filter_map(|r| match r {
                DisplayRow::Header { label, count, .. } => Some((label.as_str(), *count)),
                _ => None,
            })
            .collect();
        assert_eq!(
            headers,
            vec![
                ("Approved", Some(2)),
                ("Needs action", Some(1)),
                ("Awaiting review", Some(2)),
                ("Drafts", Some(1))
            ]
        );
        let numbers: Vec<_> = (0..d.display_len())
            .filter_map(|i| d.row(i).map(|r| r.number))
            .collect();
        assert_eq!(numbers, vec![30, 50, 10, 20, 40, 60]);
        assert_eq!(
            d.row(d.display_index_of_url(&d.rows[2].url).unwrap())
                .unwrap()
                .number,
            30
        );

        let mut review = BoardTableDelegate::new(Mode::Review, false);
        review.set_rows(vec![approved(30, Category::Done)]);
        assert!(
            matches!(&review.display[0], DisplayRow::Header { label, .. } if label == "Reviewed")
        );
    }

    #[test]
    fn approved_action_rows_lead_without_changing_notes_or_review_queue_order() {
        let mut failing = row(2, Category::Action);
        failing.review_state = ReviewState::Approved;
        failing.blockers = vec![Blocker::CiFailing];
        failing.note = "CI failing".into();
        let mut conflict = row(4, Category::Action);
        conflict.review_state = ReviewState::Approved;
        conflict.blockers = vec![Blocker::MergeConflict, Blocker::UnresolvedComments(2)];
        conflict.note = "merge conflict · 2 unresolved".into();
        let rows = vec![
            row(1, Category::Action),
            failing,
            row(3, Category::Action),
            conflict,
        ];
        let mut d = BoardTableDelegate::new(Mode::Authored, false);
        d.set_rows(rows.clone());
        assert!(
            matches!(&d.display[0], DisplayRow::Header { label, count, .. }
            if label == "Needs action" && *count == Some(4))
        );
        assert_eq!(
            (0..d.display_len())
                .filter_map(|i| d.row(i).map(|r| r.number))
                .collect::<Vec<_>>(),
            vec![2, 4, 1, 3]
        );
        assert_eq!(d.row(1).unwrap().note, "CI failing");
        assert_eq!(d.row(2).unwrap().blockers, rows[3].blockers);
        assert_eq!(d.display_index_of_url(&rows[0].url), Some(3));

        let mut review = BoardTableDelegate::new(Mode::Review, false);
        review.set_rows(
            rows.into_iter()
                .map(|mut r| {
                    r.category = Category::Todo;
                    r
                })
                .collect(),
        );
        assert_eq!(
            (0..review.display_len())
                .filter_map(|i| review.row(i).map(|r| r.number))
                .collect::<Vec<_>>(),
            vec![1, 2, 3, 4]
        );
    }

    #[test]
    fn approved_action_promotes_its_stack_but_preserves_layer_order() {
        let mut base = row(2, Category::Action);
        base.stack = Some(prmarmot_core::board::StackInfo {
            number: 70,
            size: 2,
            base_ref_name: "main".into(),
            position: Some(1),
        });
        let mut top = row(4, Category::Action);
        top.review_state = ReviewState::Approved;
        top.stack = Some(prmarmot_core::board::StackInfo {
            position: Some(2),
            ..base.stack.clone().unwrap()
        });
        let mut d = BoardTableDelegate::new(Mode::Authored, false);
        d.set_rows(vec![
            row(1, Category::Action),
            base,
            row(3, Category::Action),
            top,
        ]);
        assert_eq!(
            (0..d.display_len())
                .filter_map(|i| d.row(i).map(|r| r.number))
                .collect::<Vec<_>>(),
            vec![2, 4, 1, 3]
        );
        assert!(matches!(&d.display[1], DisplayRow::Header { label, .. } if label == "Stack #70"));
    }

    #[test]
    fn empty_groups_emit_no_header() {
        let mut d = BoardTableDelegate::new(Mode::Authored, false);
        d.set_rows(vec![row(1, Category::Await)]);
        // Only "Awaiting review" — no empty Action/Draft headers.
        assert_eq!(d.display_len(), 2);
        assert!(d.is_header(0) && !d.is_header(1));
    }

    #[test]
    fn stacks_keep_layer_order_without_mixing_action_sections() {
        let stacked = |number, position, category| {
            let mut r = row(number, category);
            r.stack = Some(prmarmot_core::board::StackInfo {
                number: 70,
                size: 3,
                base_ref_name: "main".into(),
                position: Some(position),
            });
            r
        };
        let mut d = BoardTableDelegate::new(Mode::Authored, false);
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
    fn labeled_stack_layers_keep_metadata_in_both_tabs() {
        for (mode, category) in [
            (Mode::Authored, Category::Action),
            (Mode::Review, Category::Todo),
            (Mode::Review, Category::Available),
            (Mode::Review, Category::Done),
            (Mode::Review, Category::Draft),
        ] {
            let mut upper = row(42, category);
            upper.labels = vec!["backend".into(), "bug".into()];
            upper.stack = Some(prmarmot_core::board::StackInfo {
                number: 70,
                size: 3,
                base_ref_name: "main".into(),
                position: Some(3),
            });
            let mut lower = row(57, category);
            lower.labels = vec!["frontend".into()];
            lower.stack = Some(prmarmot_core::board::StackInfo {
                position: Some(1),
                ..upper.stack.clone().unwrap()
            });
            let mut d = BoardTableDelegate::new(mode, false);
            d.set_rows(vec![upper, lower]);
            // Layer order must win over input order and PR-number order.
            assert_eq!(d.row(2).unwrap().number, 57);
            assert_eq!(d.row(3).unwrap().number, 42);
            assert_eq!(d.row(2).unwrap().labels, vec!["frontend"]);
            let upper = d.row(3).unwrap();
            assert_eq!(upper.labels, vec!["backend", "bug"]);
            assert!(filtered(upper, "backend bug"));
            assert!(!filtered(upper, "frontend"));
            assert!(detail_text(upper, Mode::Authored).contains("Labels: backend, bug"));
            assert!(detail_text(upper, Mode::Authored)
                .contains("Stack #70 · Layer 3 of 3 · Base: main"));
            assert_eq!(d.display_index_of_url(&upper.url), Some(3));
            assert!(
                matches!(&d.display[1], DisplayRow::Header { detail: Some(detail), .. } if detail == "2 of 3 layers shown")
            );
        }
    }

    #[test]
    fn available_reviews_have_a_distinct_section_and_calm_note() {
        assert_ne!(
            group_label(Mode::Review, Category::Todo, false),
            group_label(Mode::Review, Category::Available, false)
        );
        let mut r = row(1, Category::Available);
        r.note = "available for review".into();
        assert_eq!(note_presentation(&r).tone, Tone::Warning);
    }

    #[test]
    fn a_stale_wait_reads_in_local_time_in_the_details() {
        // The `is:stale` grammar itself is pinned in prmarmot-core's search
        // tests and goldens; this covers only the wording this file renders.
        let mut r = row(8, Category::Todo);
        r.title = "Fix login".into();
        r.waiting_since = Some("2026-09-12T12:00:00Z".into());
        assert!(filtered(&r, "IS:Stale login"));
        let since = DateTime::parse_from_rfc3339("2026-09-12T12:00:00Z")
            .unwrap()
            .with_timezone(&Local)
            .format("%Y-%m-%d %H:%M");
        let detail = detail_text_at(&r, Mode::Review, rule().now);
        assert!(
            detail.contains(&format!("Waiting for a reviewer for 3d (since {since})")),
            "{detail}"
        );
        assert!(!detail.contains("T12:00:00Z"), "no raw timestamp");
    }

    #[test]
    fn smallest_first_reorders_the_review_queue_and_details_name_the_size() {
        let sized = |number, additions| BoardRow {
            size: Some(ChangeSize {
                additions,
                deletions: 2,
                changed_files: 3,
            }),
            ..row(number, Category::Todo)
        };
        let mut d = BoardTableDelegate::new(Mode::Review, false);
        d.set_rows(vec![sized(1, 700), sized(2, 40)]);
        let numbers = |d: &BoardTableDelegate| -> Vec<u64> {
            (0..d.display_len())
                .filter_map(|i| d.row(i))
                .map(|r| r.number)
                .collect()
        };
        assert_eq!(numbers(&d), [1, 2]);
        d.set_sort(Sort::Smallest);
        d.set_rows(vec![sized(1, 700), sized(2, 40)]);
        assert_eq!(numbers(&d), [2, 1]);
        assert!(detail_text(&sized(2, 40), Mode::Review)
            .contains("Size: Small · 42 changed lines in 3 files (+40 −2)"));
        assert!(!detail_text(&row(3, Category::Todo), Mode::Review).contains("Size:"));
    }

    #[test]
    fn details_include_unelided_notes_and_deleted_reviewers() {
        let mut r = row(42, Category::Action);
        r.note = "merge conflict — rebase · CI failing · 3 unresolved".into();
        r.reviews = vec![prmarmot_core::board::ReviewSummary {
            login: None,
            state: "APPROVED".into(),
            submitted_at: None,
        }];
        r.stack = Some(prmarmot_core::board::StackInfo {
            number: 50,
            size: 3,
            position: Some(2),
            base_ref_name: "main".into(),
        });
        let detail = detail_text(&r, Mode::Authored);
        assert!(detail.contains(&r.note));
        assert!(detail.contains("deleted user — approved"));
        assert!(detail.contains("Stack #50 · Layer 2 of 3 · Base: main"));
    }

    #[test]
    fn details_name_your_review_only_on_review_rows_that_have_one() {
        let mut r = row(9, Category::Todo);
        let yours = |r: &BoardRow, mode| {
            detail_text(r, mode)
                .lines()
                .find(|line| line.starts_with("Your review"))
                .map(str::to_owned)
        };
        r.my_review = Some("NONE".into());
        assert_eq!(yours(&r, Mode::Review), None);
        for (state, words) in [
            ("APPROVED", "approved"),
            ("CHANGES_REQUESTED", "changes requested"),
            ("COMMENTED", "commented"),
        ] {
            r.my_review = Some(state.into());
            assert_eq!(
                yours(&r, Mode::Review).as_deref(),
                Some(&*format!("Your review: {words}"))
            );
            assert_eq!(yours(&r, Mode::Authored), None, "your own PR");
        }
        r.my_review = Some("DISMISSED".into());
        assert_eq!(yours(&r, Mode::Review), None);
        r.my_review = None;
        assert_eq!(yours(&r, Mode::Review), None);

        let review = |login: &str, state: &str| prmarmot_core::board::ReviewSummary {
            login: Some(login.into()),
            state: state.into(),
            submitted_at: None,
        };
        r.reviews = vec![
            review("alex", "APPROVED"),
            review("sam", "CHANGES_REQUESTED"),
            review("kim", "COMMENTED"),
            review("lee", "DISMISSED"),
        ];
        assert!(detail_text(&r, Mode::Authored).contains(
            "Reviews: alex — approved, sam — changes requested, kim — commented, \
             lee — dismissed"
        ));
    }

    #[test]
    fn selection_resolves_by_url_across_reorder() {
        let mut d = BoardTableDelegate::new(Mode::Authored, false);
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
                let cols = columns_for(mode, class, w, false);
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
    fn only_whole_label_chips_are_drawn_and_the_rest_fold_into_more() {
        // "+n" is 20 px wide whatever n is; chips are 4 px apart.
        let more = |_: usize| 20.0;
        let fit = |widths: &[f32], avail: f32| chips_that_fit(widths, more, avail, 4.0);
        // Everything fits: no "+n" needed.
        assert_eq!(fit(&[40.0, 40.0], 84.0), 2);
        // One px short of both: the second folds into "+1".
        assert_eq!(fit(&[40.0, 40.0], 83.0), 1);
        // One chip plus "+2" (40 + 4 + 20).
        assert_eq!(fit(&[40.0, 30.0, 30.0], 64.0), 1);
        // Not even the first chip beside "+2": the caller shortens it.
        assert_eq!(fit(&[70.0, 30.0, 30.0], 90.0), 0);
        // A single chip needs no room for "+n".
        assert_eq!(fit(&[90.0], 90.0), 1);
        assert_eq!(fit(&[], 90.0), 0);
    }

    #[test]
    fn wide_windows_give_labels_more_room() {
        for mode in [Mode::Authored, Mode::Review] {
            for all_repos in [false, true] {
                let medium = columns_for(mode, TableWidthClass::Medium, 1200.0, all_repos);
                let wide = columns_for(mode, TableWidthClass::Wide, 1360.0, all_repos);
                assert_eq!(width_of(&medium, "labels"), Some(LABELS_W));
                assert_eq!(width_of(&wide, "labels"), Some(LABELS_W_WIDE));
                // The wider column still leaves Title and Note their minimums
                // at the narrowest Wide window.
                assert!(
                    total_width(&wide) + SCROLLBAR_MARGIN <= 1360.0 + 1.0,
                    "{mode:?} all_repos={all_repos}: total {} overflows",
                    total_width(&wide)
                );
            }
        }
    }

    #[test]
    fn labels_present_except_in_compact() {
        for mode in [Mode::Authored, Mode::Review] {
            assert!(
                width_of(
                    &columns_for(mode, TableWidthClass::Compact, 1000.0, false),
                    "labels"
                )
                .is_none(),
                "{mode:?}: Labels should drop in Compact"
            );
            assert!(width_of(
                &columns_for(mode, TableWidthClass::Medium, 1200.0, false),
                "labels"
            )
            .is_some());
            assert!(width_of(
                &columns_for(mode, TableWidthClass::Wide, 1440.0, false),
                "labels"
            )
            .is_some());
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
                let cols = columns_for(mode, class, 900.0, false);
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
                let cols = columns_for(mode, class, w, false);
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
        let mut d = BoardTableDelegate::new(Mode::Authored, false);
        d.set_columns(columns_for(
            Mode::Authored,
            TableWidthClass::Wide,
            1440.0,
            false,
        ));
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
    fn all_repositories_adds_repo_without_hiding_note_at_minimum_width() {
        for mode in [Mode::Authored, Mode::Review] {
            let cols = columns_for(mode, TableWidthClass::Compact, 900.0, true);
            assert!(width_of(&cols, "repo").is_some());
            assert!(width_of(&cols, "note").unwrap() >= 224.0);
            assert!(total_width(&cols) + SCROLLBAR_MARGIN <= 900.0 + 1.0);
        }
    }

    #[test]
    fn override_storage_is_bounded_and_independent() {
        use std::collections::HashMap;
        let mut overrides: HashMap<(Mode, TableWidthClass, bool), Vec<Pixels>> = HashMap::new();
        for mode in [Mode::Authored, Mode::Review] {
            for &class in &ALL_CLASSES {
                for all_repos in [false, true] {
                    overrides.insert(
                        (mode, class, all_repos),
                        vec![px(mode as u8 as f32), px(1.0)],
                    );
                }
            }
        }
        // 2 modes × 3 classes × 2 scopes — the map can never hold more.
        assert_eq!(overrides.len(), 12);
        overrides.insert((Mode::Authored, TableWidthClass::Wide, true), vec![px(9.0)]);
        assert_eq!(overrides.len(), 12);
        // Each (mode, class, scope) layout is stored independently.
        assert_ne!(
            overrides[&(Mode::Authored, TableWidthClass::Compact, false)],
            overrides[&(Mode::Review, TableWidthClass::Compact, false)]
        );
        assert_ne!(
            overrides[&(Mode::Authored, TableWidthClass::Wide, false)],
            overrides[&(Mode::Authored, TableWidthClass::Wide, true)]
        );
    }

    /// A Load-more-sized review queue: 600 rows across repositories, with
    /// labels, authors, notes, and stacks, like the demo's scale mode.
    fn load_more_rows() -> Vec<BoardRow> {
        const WORDS: [&str; 8] = [
            "fix", "api", "cache", "login", "retry", "docs", "parser", "billing",
        ];
        (0..600u64)
            .map(|i| {
                let category = match i % 20 {
                    0 | 1 => Category::Draft,
                    n if n % 2 == 0 => Category::Todo,
                    _ => Category::Available,
                };
                let mut r = row(1000 + i, category);
                r.repo = format!("demo-labs/repo-{}", i % 6);
                r.id = format!("https://github.com/{}/pull/{}", r.repo, r.number);
                r.url = r.id.clone();
                r.title = format!(
                    "{} the {} {} path ({i})",
                    WORDS[(i % 8) as usize],
                    WORDS[(i / 8 % 8) as usize],
                    WORDS[(i / 64 % 8) as usize]
                );
                r.author = Some(["alex", "sam", "kim", "noor"][(i % 4) as usize].into());
                r.labels = match i % 5 {
                    0 => vec!["bug".into()],
                    1 => vec!["enhancement".into(), "help wanted".into()],
                    _ => Vec::new(),
                };
                r.note = "⏳ Waiting for your review · CI passing".into();
                if i % 10 < 3 {
                    r.stack = Some(prmarmot_core::board::StackInfo {
                        number: i / 10,
                        size: 3,
                        base_ref_name: "main".into(),
                        position: Some(i % 10 + 1),
                    });
                }
                r
            })
            .collect()
    }

    /// Times what one search keystroke costs before GPUI draws: the
    /// `RootView::sync_table` filter plus the display rebuild, over 600 rows.
    /// `cargo test --release --bin prmarmot keystroke_rebuild -- --ignored --nocapture`
    #[test]
    #[ignore = "timing report, not a check"]
    fn keystroke_rebuild_cost_at_load_more_size() {
        use std::time::{Duration, Instant};
        let rows = load_more_rows();
        let mut typed: Vec<String> = Vec::new();
        for query in [
            "fix api",
            "author:alex cache",
            "label:\"help wanted\" retry",
        ] {
            for end in 0..=query.len() {
                typed.push(query[..end].to_owned());
            }
            for end in (0..query.len()).rev() {
                typed.push(query[..end].to_owned());
            }
        }
        let chip = FilterChip::new(Qualifier::Label, "bug");
        let mut delegate = BoardTableDelegate::new(Mode::Review, true);
        let mut took: Vec<Duration> = Vec::new();
        let mut shown = 0;
        for pass in 0..20 {
            for (ix, query) in typed.iter().enumerate() {
                let chips: &[FilterChip] = if ix % 2 == 0 {
                    &[]
                } else {
                    std::slice::from_ref(&chip)
                };
                let started = Instant::now();
                let matching: Vec<BoardRow> = rows
                    .iter()
                    .filter(|row| {
                        filtered(row, query) && chips.iter().all(|chip| chip.matches(row, rule()))
                    })
                    .cloned()
                    .collect();
                let watched: HashSet<String> = matching
                    .iter()
                    .step_by(7)
                    .map(|row| row.id.clone())
                    .collect();
                let snoozed: HashSet<String> = matching
                    .iter()
                    .step_by(11)
                    .map(|row| row.id.clone())
                    .collect();
                shown = shown.max(matching.len());
                delegate.set_rows(matching);
                delegate.set_attention(HashMap::new(), watched, snoozed, false);
                let _ =
                    delegate.display_index_of_url("https://github.com/demo-labs/repo-5/pull/1599");
                if pass > 0 {
                    took.push(started.elapsed());
                }
            }
        }
        took.sort_unstable();
        let at = |q: f64| took[((took.len() - 1) as f64 * q).round() as usize];
        assert_eq!(shown, 600);
        println!(
            "keystroke rebuild over {} rows ({} keystrokes): p50 {:?} p90 {:?} p99 {:?} max {:?}",
            rows.len(),
            took.len(),
            at(0.5),
            at(0.9),
            at(0.99),
            at(1.0)
        );
    }
}
