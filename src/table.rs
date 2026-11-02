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

use chrono::{Local, Utc};
use gpui::prelude::FluentBuilder;
use gpui::{
    div, px, rems, AnyElement, App, ClickEvent, Context, Div, FontWeight, Hsla, InteractiveElement,
    IntoElement, Modifiers, MouseButton, ParentElement, Pixels, SharedString, Stateful,
    StatefulInteractiveElement, Styled, WeakEntity, Window,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::menu::{ContextMenuExt, DropdownMenu, PopupMenu, PopupMenuItem};
use gpui_component::table::{Column, TableDelegate, TableState};
use gpui_component::tooltip::Tooltip;
use gpui_component::{h_flex, ActiveTheme, Sizable};
use prmarmot_core::board::{BoardRow, Ci, Mode};
use prmarmot_core::cells::{
    common_labels, label_order, note_presentation, review_cell, NotePresentation, ReviewCell, Tone,
};
use prmarmot_core::layout::{
    layout_collapsible, section_explanation, section_summary, without_hidden_sections, LayoutItem,
    SectionKind, SectionOrder, Sort,
};
use prmarmot_core::pickup::{is_stale, wait_label, waiting_secs, DEFAULT_STALE_AFTER_DAYS};
use prmarmot_core::share::{share_group, ShareFormat, SharePayload};
use prmarmot_core::size::ChangeSize;
use prmarmot_core::status::EmptyAction;
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use crate::design::type_size;
use crate::design::{self, CHIP_HEIGHT, CHIP_PAD_X, CHIP_RADIUS, STATUS_DOT};
use crate::skin::{
    CAPSULE_DOT, CAPSULE_GAP, CAPSULE_HEIGHT, CAPSULE_PAD_X, CAPSULE_RADIUS, RAIL, RAIL_INSET,
};

/// One rendered line of the table: either a category section header or a PR
/// (an index into `rows`). Headers are pseudo-rows — `row()` returns `None`
/// for them, and keyboard selection bounces off them (see `app.rs`).
enum DisplayRow {
    Header {
        kind: SectionKind,
        /// A top-level section whose rows are hidden; `detail` is then its
        /// summary.
        collapsed: bool,
        label: String,
        count: Option<usize>,
        detail: Option<String>,
        /// What puts a PR in this section, shown on hover over the title.
        /// `None` for stack sub-headers.
        explanation: Option<SharedString>,
        /// PRs in a top-level group, in display order (indices into `rows`),
        /// kept even while the group is collapsed. Empty for stack sub-headers.
        members: Vec<usize>,
    },
    Pr(usize),
}

/// Viewport-width buckets that drive the responsive column layout. Kept a
/// small closed set (not raw pixels) so the per-(mode, class) manual-resize
/// overrides in `app.rs` stay bounded (3 modes × 3 classes × 2 scopes = 18).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TableWidthClass {
    Compact,
    Medium,
    Wide,
}

/// Below this the layout is Compact (Labels column dropped).
pub const COMPACT_MAX: f32 = 1120.0;
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
const PR_W: f32 = 164.0;
const CI_W: f32 = 68.0;
const UNRESOLVED_W: f32 = 92.0;
/// "+1.2k −340": two short counts side by side.
const LINES_W: f32 = 104.0;
/// An issue ID such as "ENG-1234".
const TICKET_W: f32 = 80.0;
const LABELS_W: f32 = 96.0;
/// Wide windows have room for a whole chip plus its "+n" more often.
const LABELS_W_WIDE: f32 = 120.0;
const REVIEW_W_COMPACT: f32 = 80.0;
const REVIEW_W: f32 = 190.0;
/// All open's Review column on a wide window: reviewer logins run long
/// ("copilot-pull-request-reviewer") while its Notes are short status phrases.
const REVIEW_W_ALL_OPEN_MAX: f32 = 320.0;
const AUTHOR_W_COMPACT: f32 = 80.0;
const AUTHOR_W: f32 = 116.0;
const REPO_W_COMPACT: f32 = 120.0;
const REPO_W: f32 = 168.0;
/// Title and Note never shrink below these; at the 900px floor the total still
/// fits without pushing Note offscreen.
// The extra PR action takes 24px from the title floor at narrow widths.
const TITLE_MIN: f32 = 192.0;
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

/// Horizontal padding gpui-component adds to every cell, both sides together:
/// the table is rendered `.small()`, [`design::CELL_PAD_X`] each side.
/// Subtracted when eliding cell text.
const CELL_PADDING: f32 = 2.0 * design::CELL_PAD_X;
/// The cell font size, in rems. The `.small()` table applies `text_sm()` =
/// `rems(0.875)` to every cell, *overriding* the ambient 13px — so eliding must
/// measure at this size, not the ambient one, or long labels clip their "…".
/// Keep in lockstep with the `.small()`/`TABLE_TEXT_PX` choice in `app.rs`.
const CELL_FONT_REM: f32 = 0.875;
/// `gap_1` / `gap_1p5` in px at the default 16px rem — the inter-child gaps in
/// the title/note/review cells, needed for the elision width math.
const GAP_1: f32 = 4.0;
const GAP_1P5: f32 = 6.0;
/// The least room a Note's muted tail is drawn in; less would show only "…".
const TAIL_MIN: f32 = 24.0;
/// A hair of slack so the appended "…" never lands under the cell's overflow
/// clip (our measured width can differ from the painted width by sub-pixels).
const ELIDE_SAFETY: f32 = 4.0;

/// The font size (px) cells actually render at — see `CELL_FONT_REM`.
fn cell_font_px(window: &Window) -> Pixels {
    rems(CELL_FONT_REM).to_pixels(window.rem_size())
}

/// Pixel width of `text` at the cell's rendered font size.
fn measure_width(window: &mut Window, text: &str) -> Pixels {
    measure_width_weighted(window, text, None)
}

/// The cell's text style, at `weight` when one is given: Glacier's titles
/// are a step bolder than the ambient text and are measured so.
fn cell_text_style(window: &Window, weight: Option<FontWeight>) -> gpui::TextStyle {
    let mut style = window.text_style();
    if let Some(weight) = weight {
        style.font_weight = weight;
    }
    style
}

/// [`measure_width`] at `weight`.
fn measure_width_weighted(window: &mut Window, text: &str, weight: Option<FontWeight>) -> Pixels {
    if text.is_empty() {
        return px(0.);
    }
    let font_size = cell_font_px(window);
    let run = cell_text_style(window, weight).to_run(text.len());
    window
        .text_system()
        .layout_line(text, font_size, &[run], None)
        .width
}

/// Chip text size, as [`label_chip`] renders it and measures it.
const CHIP_TEXT: Pixels = type_size::CAPTION;
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
        .layout_line(text, CHIP_TEXT, &[run], None)
        .width;
    f32::from(text_w) + chip_chrome()
}

/// `text` shortened with "…" to fit a chip's text room of `max_width`.
fn elide_chip_text(window: &mut Window, text: &str, max_width: Pixels) -> String {
    let style = chip_text_style(window);
    let runs = vec![style.to_run(text.len())];
    window
        .text_system()
        .line_wrapper(style.font(), CHIP_TEXT)
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

/// Which label chips a cell `avail` wide shows, by index into `texts`, with
/// the first one shortened when not even one fits whole, and how many fold
/// into "+n". The "+n" chips are measured only when some do not fit.
fn fit_label_chips(window: &mut Window, texts: &[String], avail: f32) -> ChipFit {
    let widths: Vec<f32> = texts.iter().map(|t| chip_width(window, t)).collect();
    let whole = widths.iter().sum::<f32>() + GAP_1 * widths.len().saturating_sub(1) as f32;
    if whole <= avail {
        return (texts.iter().cloned().enumerate().collect(), 0);
    }
    let more: Vec<f32> = (0..=texts.len())
        .map(|n| chip_width(window, &format!("+{n}")))
        .collect();
    let shown = chips_that_fit(&widths, |n| more[n], avail, GAP_1);
    let mut visible: Vec<(usize, String)> = texts.iter().cloned().enumerate().take(shown).collect();
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
    (visible, hidden)
}

/// Truncate `text` with a trailing "…" so it fits within `max_width`, at the
/// cell's rendered font size and using gpui's own line metrics. Done by hand
/// because gpui's in-cell `.truncate()` is inert inside gpui-component's
/// virtualized table: the row-measure pass shapes and caches the *untruncated*
/// line at an indefinite width, and the nowrap text cache then never
/// re-truncates at the real width. So we elide the string ourselves and render
/// a plain, already-fitting label.
fn elide(window: &mut Window, text: &str, max_width: Pixels, weight: Option<FontWeight>) -> String {
    if text.is_empty() || max_width <= px(0.) {
        return text.to_string();
    }
    let font_size = cell_font_px(window);
    let style = cell_text_style(window, weight);
    let font = style.font();
    let runs = vec![style.to_run(text.len())];
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

/// Which elided text of a row a [`CellFits`] entry holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum CellPart {
    Title,
    ReviewNames,
    NotePrimary,
    NoteTail,
}

/// Elided cell text and label-chip fits kept between frames. GPUI caches
/// shaping, but truncating a line and measuring every "+n" chip are not, and
/// the table redraws every visible cell on each frame. An entry is keyed by
/// the row's index in `rows` and holds the room it was fitted to, so it is
/// reused only for the same text at the same width. Cleared whenever rows or
/// columns change or the rem size does, and bounded by [`MAX_CELL_FITS`].
#[derive(Default)]
struct CellFits {
    rem: Pixels,
    elided: HashMap<(usize, CellPart), (String, Pixels, String)>,
    chips: HashMap<usize, (f32, ChipFit)>,
}

/// The label chips a cell shows, by index into the row's ordered labels
/// (the first possibly shortened), and how many fold into "+n".
type ChipFit = (Vec<(usize, String)>, usize);

/// Visible rows × elided parts is a few hundred; past this the cache starts
/// over rather than evicting one by one.
const MAX_CELL_FITS: usize = 4096;

impl CellFits {
    fn clear(&mut self) {
        self.elided.clear();
        self.chips.clear();
    }

    /// Start over when the rem size changed or the bound is reached.
    fn check(&mut self, window: &Window) {
        let rem = window.rem_size();
        if rem != self.rem || self.elided.len() + self.chips.len() >= MAX_CELL_FITS {
            self.rem = rem;
            self.clear();
        }
    }

    /// [`elide`], remembered for this row, part, text and room.
    fn elide(
        &mut self,
        window: &mut Window,
        key: (usize, CellPart),
        text: &str,
        room: Pixels,
        weight: Option<FontWeight>,
    ) -> String {
        self.check(window);
        if let Some((input, fitted, output)) = self.elided.get(&key) {
            if *fitted == room && input == text {
                return output.clone();
            }
        }
        let output = elide(window, text, room, weight);
        self.elided
            .insert(key, (text.to_owned(), room, output.clone()));
        output
    }
}

/// `order` with every column in it: a saved order written before a column
/// existed gets that column where the default order has it, after the column
/// it follows there.
fn complete_order(mut order: Vec<ColumnKind>) -> Vec<ColumnKind> {
    let mut seen = Vec::new();
    order.retain(|kind| {
        let first = !seen.contains(kind);
        seen.push(*kind);
        first
    });
    for (ix, kind) in ColumnKind::ALL.into_iter().enumerate() {
        if order.contains(&kind) {
            continue;
        }
        let at = ColumnKind::ALL[..ix]
            .iter()
            .rev()
            .find_map(|before| order.iter().position(|k| k == before))
            .map_or(0, |position| position + 1);
        order.insert(at, kind);
    }
    order
}

/// `order` with the columns in `on_screen` put in their on-screen order, in
/// the places those columns held; the others stay where they were.
fn merged_order(order: &[ColumnKind], on_screen: &[ColumnKind]) -> Vec<ColumnKind> {
    let mut moved = on_screen.iter();
    complete_order(order.to_vec())
        .into_iter()
        .map(|kind| {
            if on_screen.contains(&kind) {
                moved.next().copied().unwrap_or(kind)
            } else {
                kind
            }
        })
        .collect()
}

/// A line count short enough for the Lines column: 999, 1.2k, 12k, 1.2M.
fn short_count(n: u64) -> String {
    let short = |value: f64, unit: &str| {
        if value < 10.0 {
            format!("{:.1}{unit}", (value * 10.0).floor() / 10.0).replace(".0", "")
        } else {
            format!("{}{unit}", value.floor())
        }
    };
    match n {
        0..=999 => n.to_string(),
        1_000..=999_999 => short(n as f64 / 1_000.0, "k"),
        _ => short(n as f64 / 1_000_000.0, "M"),
    }
}

/// What a column shows. `render_td` matches it exhaustively, so a column
/// without a cell is a compile error rather than a silently blank column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnKind {
    Pr,
    Repo,
    /// The linked issue (`[issue_link]`, e.g. a Linear ticket).
    Ticket,
    Title,
    Ci,
    Review,
    Author,
    Unresolved,
    /// Lines added and removed.
    Lines,
    Labels,
    Note,
}

impl ColumnKind {
    const ALL: [Self; 11] = [
        Self::Pr,
        Self::Repo,
        Self::Ticket,
        Self::Title,
        Self::Ci,
        Self::Review,
        Self::Author,
        Self::Unresolved,
        Self::Lines,
        Self::Labels,
        Self::Note,
    ];

    /// The column's key: stable, and what saved width overrides refer to.
    pub fn key(self) -> &'static str {
        match self {
            Self::Pr => "pr",
            Self::Repo => "repo",
            Self::Ticket => "ticket",
            Self::Title => "title",
            Self::Ci => "ci",
            Self::Review => "review",
            Self::Author => "author",
            Self::Unresolved => "unresolved",
            Self::Lines => "lines",
            Self::Labels => "labels",
            Self::Note => "note",
        }
    }

    fn header(self) -> &'static str {
        match self {
            Self::Pr => "PR",
            Self::Repo => "Repo",
            Self::Ticket => "Ticket",
            Self::Title => "Title",
            Self::Ci => "CI",
            Self::Review => "Review",
            Self::Author => "Author",
            // Counts open review threads, not individual replies.
            Self::Unresolved => "Unresolved",
            Self::Lines => "Lines",
            Self::Labels => "Labels",
            Self::Note => "Note",
        }
    }

    /// The kind of a column [`columns_for`] made.
    pub fn of(column: &Column) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|kind| column.key.as_ref() == kind.key())
    }

    fn column(self, width: f32) -> Column {
        Column::new(self.key(), self.header()).width(px(width))
    }
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
    ticket: bool,
) -> Vec<Column> {
    let compact = class == TableWidthClass::Compact;
    let show_labels = !compact;
    let review_w = if compact {
        REVIEW_W_COMPACT
    } else if all_repos && class == TableWidthClass::Medium {
        160.0
    } else {
        REVIEW_W
    };
    let author_w = if compact { AUTHOR_W_COMPACT } else { AUTHOR_W };
    // Lines needs a wide window; narrower ones add it from the header menu.
    let show_lines = class == TableWidthClass::Wide;
    let lines_w = if show_lines { LINES_W } else { 0.0 };
    let labels_w = match class {
        TableWidthClass::Compact => 0.0,
        TableWidthClass::Medium => LABELS_W,
        TableWidthClass::Wide => LABELS_W_WIDE,
    };
    // All open carries Author and Review side by side, so Review narrows at
    // medium widths and folds into the Note (which states the review) when
    // compact.
    let mut all_open_review_w = match class {
        TableWidthClass::Compact => 0.0,
        TableWidthClass::Medium => REVIEW_W_COMPACT,
        TableWidthClass::Wide => REVIEW_W,
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

    let mut fixed_sum = match mode {
        Mode::Authored => PR_W + CI_W + review_w + UNRESOLVED_W + lines_w + labels_w + repo_w,
        Mode::Review => PR_W + CI_W + author_w + UNRESOLVED_W + lines_w + labels_w + repo_w,
        Mode::AllOpen => {
            PR_W + CI_W + author_w + all_open_review_w + UNRESOLVED_W + lines_w + labels_w + repo_w
        }
    };
    // Elastic remainder, floored so Title/Note always meet their minimums.
    // Every repository adds the Repo column, which the full minimums don't
    // leave room for at the narrowest Compact or Medium table (a Medium table
    // can be as narrow as 1120px beside the Details panel); Title keeps room for
    // the row action controls.
    let title_min = if all_repos { 120.0 } else { TITLE_MIN };
    let note_min = if all_repos { 224.0 } else { NOTE_MIN };
    // With an issue rule configured, the Ticket column shows on a wide window
    // that keeps Title and Note their minimums beside it.
    let show_ticket = ticket
        && class == TableWidthClass::Wide
        && viewport_width - fixed_sum - TICKET_W - SCROLLBAR_MARGIN >= title_min + note_min;
    if show_ticket {
        fixed_sum += TICKET_W;
    }
    let flexible = (viewport_width - fixed_sum - SCROLLBAR_MARGIN).max(title_min + note_min);
    let mut title_w = ((flexible * TITLE_FLEX_RATIO / QUANTUM).round() * QUANTUM)
        .max(title_min)
        .min(flexible - note_min);
    let mut note_w = (flexible - title_w).max(note_min);
    if note_w < title_w {
        // Never let Title out-grow Note — Note is the product.
        title_w = note_w;
    }
    // All open on a wide window: Review takes what the Note has beyond the
    // Title's width, never the Title's share, so a long reviewer login stops
    // being cut off beside a Note column that is mostly empty.
    if mode == Mode::AllOpen && class == TableWidthClass::Wide {
        let spare = note_w - title_w.max(note_min);
        let target = (REVIEW_W + spare.max(0.0)).min(REVIEW_W_ALL_OPEN_MAX);
        let review_w = ((target / QUANTUM).floor() * QUANTUM).max(REVIEW_W);
        note_w -= review_w - all_open_review_w;
        all_open_review_w = review_w;
    }

    let col = |kind: ColumnKind, w: f32| kind.column(w);
    match mode {
        Mode::Authored => {
            let mut cols = vec![col(ColumnKind::Pr, PR_W)];
            if all_repos {
                cols.push(col(ColumnKind::Repo, repo_w));
            }
            if show_ticket {
                cols.push(col(ColumnKind::Ticket, TICKET_W));
            }
            cols.extend([
                col(ColumnKind::Title, title_w),
                col(ColumnKind::Ci, CI_W),
                col(ColumnKind::Review, review_w),
                col(ColumnKind::Unresolved, UNRESOLVED_W),
            ]);
            if show_lines {
                cols.push(col(ColumnKind::Lines, lines_w));
            }
            if show_labels {
                cols.push(col(ColumnKind::Labels, labels_w));
            }
            cols.push(col(ColumnKind::Note, note_w));
            cols
        }
        Mode::Review => {
            let mut cols = vec![col(ColumnKind::Pr, PR_W)];
            if all_repos {
                cols.push(col(ColumnKind::Repo, repo_w));
            }
            if show_ticket {
                cols.push(col(ColumnKind::Ticket, TICKET_W));
            }
            cols.extend([
                col(ColumnKind::Title, title_w),
                col(ColumnKind::Ci, CI_W),
                col(ColumnKind::Author, author_w),
                col(ColumnKind::Unresolved, UNRESOLVED_W),
            ]);
            if show_lines {
                cols.push(col(ColumnKind::Lines, lines_w));
            }
            if show_labels {
                cols.push(col(ColumnKind::Labels, labels_w));
            }
            cols.push(col(ColumnKind::Note, note_w));
            cols
        }
        // Everyone's PRs: who wrote it, and where its review stands. The
        // author and label cells filter the whole repository when clicked.
        Mode::AllOpen => {
            let mut cols = vec![col(ColumnKind::Pr, PR_W)];
            if show_ticket {
                cols.push(col(ColumnKind::Ticket, TICKET_W));
            }
            cols.extend([
                col(ColumnKind::Title, title_w),
                col(ColumnKind::Ci, CI_W),
                col(ColumnKind::Author, author_w),
                col(ColumnKind::Unresolved, UNRESOLVED_W),
            ]);
            if show_lines {
                cols.push(col(ColumnKind::Lines, lines_w));
            }
            if !compact {
                cols.push(col(ColumnKind::Review, all_open_review_w));
            }
            if show_labels {
                cols.push(col(ColumnKind::Labels, labels_w));
            }
            cols.push(col(ColumnKind::Note, note_w));
            cols
        }
    }
}

// The search grammar lives in `prmarmot-core` so the desktop app and the iPad
// cannot drift on what `label:"help wanted"` means. Only the drawing stays
// here.
pub use prmarmot_core::search::{take_filter_chips, with_filter, FilterChip, Qualifier, StaleRule};

/// One neutral chip style for labels (spec §7): GitHub's arbitrary label
/// hues would out-shout the status system. Also used for the search tokens.
/// Glacier's chip is an opaque tint of the text's ink with an edge a shade
/// stronger, so it stays visible on a hovered or selected row (critique C5:
/// a `muted` fill vanished on a `muted` hover).
pub fn label_chip(theme: &gpui_component::Theme, glacier: bool, cx: &App) -> Div {
    h_flex()
        .h(px(CHIP_HEIGHT))
        .px(px(CHIP_PAD_X))
        .items_center()
        .rounded(px(CHIP_RADIUS))
        .bg(if glacier {
            crate::skin::chip_bg(cx)
        } else {
            theme.muted
        })
        .border_1()
        .border_color(if glacier {
            crate::skin::chip_edge(cx)
        } else {
            theme.border
        })
        .text_size(type_size::CAPTION)
        .font_weight(FontWeight::MEDIUM)
        .whitespace_nowrap()
}

/// Core formats the waiting timestamp with a fixed offset. Resolve it at
/// that instant, not today: daylight saving may have changed since then.
pub(crate) fn waiting_offset_secs(row: &BoardRow) -> i32 {
    row.waiting_since
        .as_deref()
        .and_then(|since| chrono::DateTime::parse_from_rfc3339(since).ok())
        .map(|since| since.with_timezone(&Local).offset().local_minus_utc())
        .unwrap_or(0) // No valid timestamp means core omits the waiting line.
}

/// "Small · 42 changed lines in 3 files (+30 −12)".
fn size_text(size: ChangeSize) -> String {
    prmarmot_core::detail::size_text(size)
}

/// Full, unelided snapshot details; no secondary network request or hidden
/// cache. The lines themselves are core's, so the iPad shows the same ones.
#[cfg(test)]
pub fn detail_text(row: &BoardRow, mode: Mode) -> String {
    detail_text_at(row, mode, Utc::now())
}

/// [`detail_text`] with the wait measured at `now`.
#[cfg(test)]
fn detail_text_at(row: &BoardRow, mode: Mode, now: chrono::DateTime<Utc>) -> String {
    prmarmot_core::detail::detail_text(row, mode, now, waiting_offset_secs(row))
}

#[derive(Clone)]
pub enum RowAction {
    Open,
    OpenVscode,
    ReviewVscode,
    Copy(String),
    Details,
    Watch,
    Snooze,
    CancelSnooze,
    /// Read the PR's Linear ticket again instead of the cached one.
    RefreshTicket,
}

pub fn row_copy_items(row: &BoardRow, mode: Mode) -> Vec<(&'static str, String)> {
    prmarmot_core::detail::copy_items(row, mode, Utc::now(), waiting_offset_secs(row))
}

/// A row's action, by PR id: the row is looked up when it runs, so a frame
/// never copies rows into closures it may not call.
type RowActionHandler = std::rc::Rc<dyn Fn(String, RowAction, &mut Window, &mut App)>;
type FilterClickHandler = Rc<dyn Fn(FilterChip, &mut Window, &mut App)>;
type GroupCopyHandler = Rc<dyn Fn(GroupCopy, &mut Window, &mut App)>;
type ColumnToggleHandler = Rc<dyn Fn(ColumnKind, &mut Window, &mut App)>;
type SectionToggleHandler = Rc<dyn Fn(SectionKind, &mut Window, &mut App)>;
type EmptyActionHandler = Rc<dyn Fn(EmptyAction, &mut Window, &mut App)>;
type SelectionActionHandler = Rc<dyn Fn(SelectionAction, &mut Window, &mut App)>;

/// How the next `SelectRow` treats the rows already selected. Recorded at
/// mouse-down, because the framework's click event carries no modifiers, and
/// by the app before a select it makes itself; each for one display row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClickIntent {
    /// Select this row alone.
    Plain,
    /// ⌘-click (Ctrl on Linux): add or remove this row.
    Toggle,
    /// ⇧-click, ⇧↑/↓: every PR between the anchor and this row.
    Range,
    /// The app moved the caret itself (a restore after a refresh, a header
    /// bounce): the selection stays as it is.
    Keep,
}

impl ClickIntent {
    fn from_modifiers(modifiers: Modifiers) -> Self {
        if modifiers.shift {
            Self::Range
        } else if modifiers.secondary() {
            Self::Toggle
        } else {
            Self::Plain
        }
    }
}

/// One action over every selected row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionAction {
    /// On GitHub, at most [`prmarmot_core::status::MAX_OPEN_TOGETHER`].
    Open,
    Copy(ShareFormat),
    Watch,
    Unwatch,
    Snooze,
    CancelSnooze,
    RefreshTickets,
}

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
/// A PR row, for the controls it reveals on hover (the watch toggle).
const ROW_GROUP: &str = "board-pr-row";

/// The marks the table draws beside rows: changed (with the marker's
/// tooltip), watched, snoozed, and the folded sections.
#[derive(Default)]
pub struct RowAttention {
    pub changed: HashMap<String, SharedString>,
    pub watched: HashSet<String>,
    pub snoozed: HashSet<String>,
    pub collapsed: Vec<SectionKind>,
}

pub struct BoardTableDelegate {
    rows: Vec<BoardRow>,
    display: Vec<DisplayRow>,
    /// Bumped with every `rebuild_display`; see `display_version`.
    display_version: u64,
    columns: Vec<Column>,
    /// Explicit column selection, retained across responsive layouts and queues.
    visible_columns: Option<Vec<ColumnKind>>,
    /// The order the headers were dragged into, every column included; `None`
    /// keeps each view's own order.
    column_order: Option<Vec<ColumnKind>>,
    pub on_column_toggle: Option<ColumnToggleHandler>,
    mode: Mode,
    all_repos: bool,
    /// Changed-marker tooltip text by PR id; present only for changed PRs.
    changed: HashMap<String, SharedString>,
    watched: HashSet<String>,
    snoozed: HashSet<String>,
    /// Top-level sections whose rows are hidden (Snoozed by default).
    collapsed: Vec<SectionKind>,
    /// A wait this many days long shows as stale.
    stale_after_days: u64,
    /// The order inside the review queue's pickup sections.
    sort: Sort,
    /// The order of the sections themselves (`section_order`).
    sections: SectionOrder,
    /// The sections left out of every view (`hidden_sections`).
    hidden_sections: Vec<SectionKind>,
    /// All open only: labels most rows on screen carry, drawn after the
    /// ones that tell rows apart. Empty in the other views.
    common_labels: Vec<String>,
    pub on_row_action: Option<RowActionHandler>,
    pub on_group_copy: Option<GroupCopyHandler>,
    /// A label, author, or repository was clicked: add it to the search.
    pub on_filter_click: Option<FilterClickHandler>,
    /// A section header was clicked: collapse or expand that section.
    pub on_section_toggle: Option<SectionToggleHandler>,
    /// The empty board's one button (Show all repositories, Load more).
    pub on_empty_action: Option<EmptyActionHandler>,
    /// A right-click on a selected row chose an action for the selection.
    pub on_selection_action: Option<SelectionActionHandler>,
    /// Several rows selected at once (⇧-click, ⌘-click, ⇧↑/↓), by PR id,
    /// the caret row included; empty while the caret row is the whole
    /// selection. Ids, not indices, so a refresh that reorders rows keeps
    /// the same PRs selected.
    multi: HashSet<String>,
    /// The modifiers of the last mouse-down on a PR row, with the row, read
    /// once by the `SelectRow` that follows it.
    click_intent: Rc<Cell<Option<(usize, ClickIntent)>>>,
    /// GitHub has another page for this view, for the empty board's copy.
    can_load_more: bool,
    fits: CellFits,
    /// What each of `columns` shows, read once per column set.
    kinds: Vec<Option<ColumnKind>>,
    /// Display index of the header whose Copy menu is open, so its button
    /// stays visible while the pointer is over the menu instead of the header.
    copy_menu_open: Rc<Cell<Option<usize>>>,
    /// The caret row (the table's selected row), for Glacier's rail; the
    /// framework paints the caret's fill itself.
    caret: Option<usize>,
}

impl BoardTableDelegate {
    pub fn new(mode: Mode, all_repos: bool) -> Self {
        let columns = columns_for(
            mode,
            TableWidthClass::Medium,
            DEFAULT_VIEWPORT_WIDTH,
            all_repos,
            false,
        );
        Self {
            rows: Vec::new(),
            display: Vec::new(),
            display_version: 0,
            kinds: columns.iter().map(ColumnKind::of).collect(),
            columns,
            visible_columns: None,
            column_order: None,
            on_column_toggle: None,
            mode,
            all_repos,
            changed: HashMap::new(),
            watched: HashSet::new(),
            snoozed: HashSet::new(),
            collapsed: vec![SectionKind::Snoozed],
            stale_after_days: DEFAULT_STALE_AFTER_DAYS,
            sort: Sort::Wait,
            sections: SectionOrder::default(),
            hidden_sections: Vec::new(),
            common_labels: Vec::new(),
            on_row_action: None,
            on_group_copy: None,
            on_filter_click: None,
            on_section_toggle: None,
            on_empty_action: None,
            on_selection_action: None,
            multi: HashSet::new(),
            click_intent: Rc::new(Cell::new(None)),
            can_load_more: false,
            fits: CellFits::default(),
            copy_menu_open: Rc::new(Cell::new(None)),
            caret: None,
        }
    }

    /// Switch queue: change the sort/grouping mode. Like the scope, it takes
    /// effect with the next [`Self::set_rows_and_attention`], which the
    /// switch's own row sync runs, so a switch lays the table out once.
    /// Columns are owned by `RootView` (they depend on the live window width),
    /// so it calls `set_columns` right after this.
    pub fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
    }

    /// Takes effect with the next [`Self::set_rows_and_attention`].
    pub fn set_scope(&mut self, all_repos: bool) {
        self.all_repos = all_repos;
    }

    /// Replace the whole column set (responsive relayout or a mode switch).
    pub fn set_columns(&mut self, columns: Vec<Column>) {
        self.kinds = columns.iter().map(ColumnKind::of).collect();
        self.columns = columns;
        self.fits.clear();
    }

    pub fn restore_visible_columns(&mut self, keys: Option<&[String]>) {
        self.visible_columns = keys.and_then(|keys| {
            let visible: Vec<_> = ColumnKind::ALL
                .into_iter()
                .filter(|kind| keys.iter().any(|key| key == kind.key()))
                .collect();
            // Ignore obsolete keys; an empty selection must not strand the menu.
            (!visible.is_empty()).then_some(visible)
        });
    }

    pub fn visible_column_keys(&self) -> Vec<String> {
        self.visible_columns
            .as_deref()
            .unwrap_or(&[])
            .iter()
            .map(|kind| kind.key().to_owned())
            .collect()
    }

    pub fn toggle_column(&mut self, kind: ColumnKind) {
        let visible = self
            .visible_columns
            .get_or_insert_with(|| self.kinds.iter().flatten().copied().collect());
        if visible.contains(&kind) {
            if visible.len() > 1 {
                visible.retain(|column| *column != kind);
            }
        } else {
            visible.push(kind);
        }
    }

    pub fn restore_column_order(&mut self, keys: Option<&[String]>) {
        self.column_order = keys.map(|keys| {
            complete_order(
                keys.iter()
                    .filter_map(|key| ColumnKind::ALL.into_iter().find(|kind| kind.key() == key))
                    .collect(),
            )
        });
    }

    /// Take the order the columns on screen were just dragged into as the
    /// order for every view, the columns not on screen keeping their places,
    /// and return it as the keys to save.
    pub fn remember_column_order(&mut self) -> Vec<String> {
        let on_screen: Vec<ColumnKind> = self.kinds.iter().flatten().copied().collect();
        let order = merged_order(
            self.column_order.as_deref().unwrap_or(&ColumnKind::ALL),
            &on_screen,
        );
        let keys = order.iter().map(|kind| kind.key().to_owned()).collect();
        self.column_order = Some(order);
        keys
    }

    /// The columns to show: the visible selection, if one was made, in the
    /// dragged order, if there is one.
    pub fn customize_columns(&self, columns: Vec<Column>) -> Vec<Column> {
        let mut columns = self.selected_columns(columns);
        if let Some(order) = &self.column_order {
            columns.sort_by_key(|column| {
                ColumnKind::of(column)
                    .and_then(|kind| order.iter().position(|k| *k == kind))
                    .unwrap_or(usize::MAX)
            });
        }
        columns
    }

    fn selected_columns(&self, mut columns: Vec<Column>) -> Vec<Column> {
        let Some(visible) = &self.visible_columns else {
            return columns;
        };
        // Keep responsive widths where available and supply widths for columns
        // that this queue or window size normally omits.
        ColumnKind::ALL
            .into_iter()
            .filter(|kind| visible.contains(kind))
            .map(|kind| {
                if let Some(index) = columns
                    .iter()
                    .position(|column| ColumnKind::of(column) == Some(kind))
                {
                    columns.remove(index)
                } else {
                    let width = match kind {
                        ColumnKind::Pr => PR_W,
                        ColumnKind::Repo => REPO_W,
                        ColumnKind::Ticket => TICKET_W,
                        ColumnKind::Title => TITLE_MIN,
                        ColumnKind::Ci => CI_W,
                        ColumnKind::Review => REVIEW_W,
                        ColumnKind::Author => AUTHOR_W,
                        ColumnKind::Unresolved => UNRESOLVED_W,
                        ColumnKind::Lines => LINES_W,
                        ColumnKind::Labels => LABELS_W,
                        ColumnKind::Note => NOTE_MIN,
                    };
                    kind.column(width)
                }
            })
            .collect()
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
        self.fits.clear();
        true
    }

    /// The caret row, from the table's `SelectRow` / `ClearSelection`.
    pub fn set_caret(&mut self, ix: Option<usize>) {
        self.caret = ix;
    }

    /// Forget every fitted text and chip: the look changed what they measure.
    pub fn clear_fits(&mut self) {
        self.fits.clear();
    }

    pub fn set_stale_after_days(&mut self, days: u64) {
        self.stale_after_days = days;
    }

    /// Whether GitHub has another page for this view; the empty board says
    /// so and offers Load more.
    pub fn set_can_load_more(&mut self, can_load_more: bool) {
        self.can_load_more = can_load_more;
    }

    /// Takes effect with the next `set_rows`.
    pub fn set_sort(&mut self, sort: Sort) {
        self.sort = sort;
    }

    /// Takes effect with the next `set_rows`.
    pub fn set_section_order(&mut self, sections: SectionOrder) {
        self.sections = sections;
    }

    /// Takes effect with the next [`Self::set_rows_and_attention`].
    pub fn set_hidden_sections(&mut self, hidden: Vec<SectionKind>) {
        self.hidden_sections = hidden;
    }

    #[cfg(test)]
    fn set_rows(&mut self, rows: Vec<BoardRow>) {
        self.rows = rows;
        self.rebuild_display();
        self.prune_multi();
    }

    #[cfg(test)]
    fn set_attention(
        &mut self,
        changed: HashMap<String, SharedString>,
        watched: HashSet<String>,
        snoozed: HashSet<String>,
        collapsed: Vec<SectionKind>,
    ) {
        self.changed = changed;
        self.watched = watched;
        self.snoozed = snoozed;
        self.collapsed = collapsed;
        self.rebuild_display();
    }

    /// The rows to show and their marks, laid out once.
    pub fn set_rows_and_attention(&mut self, rows: Vec<BoardRow>, attention: RowAttention) {
        self.rows = rows;
        self.changed = attention.changed;
        self.watched = attention.watched;
        self.snoozed = attention.snoozed;
        self.collapsed = attention.collapsed;
        self.rebuild_display();
        self.prune_multi();
    }

    /// A selected PR that left the board leaves the selection; one left
    /// alone is the caret row again.
    fn prune_multi(&mut self) {
        let ids: HashSet<&str> = self.rows.iter().map(|row| row.id.as_str()).collect();
        self.multi.retain(|id| ids.contains(id.as_str()));
        if self.multi.len() <= 1 {
            self.multi.clear();
        }
    }

    // -- Several rows at once -------------------------------------------

    /// Record how the app's own next select of `row_ix` treats the selection.
    pub fn set_click_intent(&self, row_ix: usize, intent: ClickIntent) {
        self.click_intent.set(Some((row_ix, intent)));
    }

    /// The intent recorded for `row_ix`, consumed; `Plain` when none was.
    pub fn take_click_intent(&self, row_ix: usize) -> ClickIntent {
        match self.click_intent.take() {
            Some((ix, intent)) if ix == row_ix => intent,
            _ => ClickIntent::Plain,
        }
    }

    pub fn multi_active(&self) -> bool {
        !self.multi.is_empty()
    }

    pub fn multi_len(&self) -> usize {
        self.multi.len()
    }

    #[cfg(test)]
    pub fn in_multi(&self, pr_id: &str) -> bool {
        self.multi.contains(pr_id)
    }

    /// The selected rows in display order.
    pub fn multi_rows(&self) -> Vec<BoardRow> {
        self.display
            .iter()
            .filter_map(|d| match d {
                DisplayRow::Pr(i) => self.rows.get(*i).filter(|row| self.multi.contains(&row.id)),
                DisplayRow::Header { .. } => None,
            })
            .cloned()
            .collect()
    }

    pub fn set_multi(&mut self, ids: impl IntoIterator<Item = String>) {
        self.multi = ids.into_iter().collect();
        if self.multi.len() <= 1 {
            self.multi.clear();
        }
    }

    /// Back to the caret row alone. True when there was more.
    pub fn clear_multi(&mut self) -> bool {
        let had = !self.multi.is_empty();
        self.multi.clear();
        had
    }

    /// ⌘-click: add `pr_id`, or remove it. A single caret row (`caret`)
    /// joins first, so the selection grows from where it was. Returns
    /// whether `pr_id` is selected afterwards.
    pub fn toggle_multi(&mut self, pr_id: &str, caret: Option<&str>) -> bool {
        if self.multi.is_empty() {
            if let Some(caret) = caret {
                self.multi.insert(caret.to_owned());
            }
        }
        if self.multi.remove(pr_id) {
            return false;
        }
        self.multi.insert(pr_id.to_owned());
        true
    }

    /// The PR ids from one display row to another, both included, in
    /// display order; headers and folded sections hold no rows to select.
    pub fn ids_between(&self, a: usize, b: usize) -> Vec<String> {
        let (from, to) = if a <= b { (a, b) } else { (b, a) };
        self.display
            .get(from..=to)
            .unwrap_or_default()
            .iter()
            .filter_map(|d| match d {
                DisplayRow::Pr(i) => self.rows.get(*i).map(|row| row.id.clone()),
                DisplayRow::Header { .. } => None,
            })
            .collect()
    }

    /// Every PR on screen, folded sections excluded: ⌘A.
    pub fn visible_pr_ids(&self) -> Vec<String> {
        self.ids_between(0, self.display.len().saturating_sub(1))
    }

    /// The selected display row nearest `from`, for the caret after the row
    /// it sat on was deselected.
    pub fn nearest_multi_index(&self, from: usize) -> Option<usize> {
        self.display
            .iter()
            .enumerate()
            .filter(|(_, d)| match d {
                DisplayRow::Pr(i) => self
                    .rows
                    .get(*i)
                    .is_some_and(|row| self.multi.contains(&row.id)),
                DisplayRow::Header { .. } => false,
            })
            .min_by_key(|(ix, _)| ix.abs_diff(from))
            .map(|(ix, _)| ix)
    }

    /// The selection for the clipboard, under the shared selection heading.
    pub fn selection_copy(&self, format: ShareFormat) -> Option<GroupCopy> {
        let rows = self.multi_rows();
        if rows.is_empty() {
            return None;
        }
        Some(GroupCopy {
            format,
            count: rows.len(),
            payload: share_group(
                prmarmot_core::share::SELECTION_TITLE,
                &rows,
                self.mode,
                format,
            ),
        })
    }

    /// The right-click menu of a row that is part of the selection: every
    /// item acts on all of it, and says how many that is.
    fn selection_menu(&self, mut menu: PopupMenu) -> PopupMenu {
        let Some(handler) = self.on_selection_action.clone() else {
            return menu;
        };
        let rows = self.multi_rows();
        let n = rows.len();
        let unwatched = rows
            .iter()
            .filter(|row| !self.watched.contains(&row.id))
            .count();
        let snoozed = rows
            .iter()
            .filter(|row| self.snoozed.contains(&row.id))
            .count();
        let open = if n > prmarmot_core::status::MAX_OPEN_TOGETHER {
            format!(
                "Open the first {} of {n} on GitHub",
                prmarmot_core::status::MAX_OPEN_TOGETHER
            )
        } else {
            format!("Open {n} PRs on GitHub")
        };
        let mut items: Vec<(String, SelectionAction)> = vec![(open, SelectionAction::Open)];
        items.extend(
            GROUP_COPY_ITEMS
                .iter()
                .map(|(label, format)| (format!("{label} ({n})"), SelectionAction::Copy(*format))),
        );
        if unwatched > 0 {
            items.push((format!("Watch {unwatched} PRs"), SelectionAction::Watch));
        }
        if unwatched < n {
            items.push((
                format!("Unwatch {} PRs", n - unwatched),
                SelectionAction::Unwatch,
            ));
        }
        items.push((format!("Snooze {n} PRs…"), SelectionAction::Snooze));
        if snoozed > 0 {
            items.push((
                format!("Cancel snooze for {snoozed} PRs"),
                SelectionAction::CancelSnooze,
            ));
        }
        for (index, (label, action)) in items.into_iter().enumerate() {
            if index == 1 || index == 5 {
                menu = menu.separator();
            }
            let handler = handler.clone();
            menu = menu.item(PopupMenuItem::new(label).on_click(move |_, window, cx| {
                handler(action, window, cx);
            }));
        }
        menu.separator().item(
            PopupMenuItem::new(format!("Refresh Linear tickets ({n})")).on_click(
                move |_, window, cx| handler(SelectionAction::RefreshTickets, window, cx),
            ),
        )
    }

    /// Rebuild display rows from the shared core layout (sections, the Approved
    /// split, stacks, Snoozed), adding the window's own Snoozed wording.
    fn rebuild_display(&mut self) {
        self.display_version = self.display_version.wrapping_add(1);
        // Fits are keyed by the index into `rows`, which may now be another PR.
        self.fits.clear();
        // The caret is a display index too: it follows its PR, or goes.
        let caret_url = self
            .caret
            .and_then(|ix| self.row(ix))
            .map(|row| row.url.clone());
        let now = Utc::now();
        self.common_labels = if self.mode == Mode::AllOpen {
            common_labels(&self.rows)
        } else {
            Vec::new()
        };
        self.display = without_hidden_sections(
            layout_collapsible(
                &self.rows,
                self.mode,
                self.all_repos,
                &self.snoozed,
                &self.collapsed,
                self.sort,
                &self.sections,
            ),
            &self.hidden_sections,
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
            } => {
                let collapsed = count.is_some() && self.collapsed.contains(&kind);
                DisplayRow::Header {
                    kind,
                    collapsed,
                    label,
                    count,
                    explanation: section_explanation(self.mode, kind, self.all_repos)
                        .map(Into::into),
                    // Collapsed, a section says what it holds instead.
                    detail: if collapsed {
                        section_summary(
                            self.mode,
                            kind,
                            members.iter().filter_map(|&ix| self.rows.get(ix)),
                            now,
                        )
                    } else {
                        detail
                    },
                    members,
                }
            }
        })
        .collect();
        self.caret = caret_url.and_then(|url| self.display_index_of_url(&url));
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
                || next.stack.as_ref().is_none_or(|next_stack| {
                    next_stack.number != stack.number
                        || next_stack.branch.is_some() != stack.branch.is_some()
                })
        })
    }

    /// The `BoardRow` at a display index, or `None` if it is a section header.
    /// The shown row with this PR id.
    pub fn row_by_id(&self, pr_id: &str) -> Option<&BoardRow> {
        self.rows.iter().find(|row| row.id == pr_id)
    }

    pub fn row(&self, display_ix: usize) -> Option<&BoardRow> {
        match self.display.get(display_ix)? {
            DisplayRow::Pr(i) => self.rows.get(*i),
            DisplayRow::Header { .. } => None,
        }
    }

    /// True when the display index is a section header.
    #[cfg(test)]
    pub fn is_header(&self, display_ix: usize) -> bool {
        matches!(
            self.display.get(display_ix),
            Some(DisplayRow::Header { .. })
        )
    }

    /// Whether selection may rest on this display index: a PR, or a folded
    /// section's header, so `c` can unfold it from the keyboard. Every other
    /// header is skipped.
    pub fn is_selectable(&self, display_ix: usize) -> bool {
        matches!(
            self.display.get(display_ix),
            Some(DisplayRow::Pr(_))
                | Some(DisplayRow::Header {
                    collapsed: true,
                    ..
                })
        )
    }

    /// The section whose folded header is at this display index.
    pub fn folded_section_at(&self, display_ix: usize) -> Option<SectionKind> {
        match self.display.get(display_ix)? {
            DisplayRow::Header {
                kind,
                collapsed: true,
                ..
            } => Some(*kind),
            _ => None,
        }
    }

    /// The top-level section a display row belongs to (a stack sub-header
    /// belongs to the section above it).
    pub fn section_at(&self, display_ix: usize) -> Option<SectionKind> {
        self.display
            .get(..=display_ix)?
            .iter()
            .rev()
            .find_map(|d| match d {
                DisplayRow::Header {
                    kind,
                    count: Some(_),
                    ..
                } => Some(*kind),
                _ => None,
            })
    }

    /// The display index of a top-level section's header.
    pub fn header_index(&self, kind: SectionKind) -> Option<usize> {
        self.display.iter().position(
            |d| matches!(d, DisplayRow::Header { kind: k, count: Some(_), .. } if *k == kind),
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

/// The Note's dot and primary-text colours for one of core's tones: only an
/// exceptional blocker is red; routine notes get a muted dot; a draft none.
/// The table's Note cell and the Details Note line agree through this.
pub(crate) fn note_tone_colors(
    tone: Tone,
    theme: &gpui_component::theme::Theme,
) -> (Option<Hsla>, Hsla) {
    let muted = theme.muted_foreground;
    match tone {
        Tone::Danger => (Some(theme.danger), theme.danger),
        Tone::Warning => (Some(theme.warning), muted),
        Tone::Success => (Some(theme.success), muted),
        Tone::Routine => (Some(muted), muted),
        Tone::Muted => (None, muted),
    }
}

pub(crate) fn status_dot(color: Hsla) -> Div {
    div()
        .size(px(STATUS_DOT))
        .rounded_full()
        .flex_shrink_0()
        .bg(color)
}

/// Glacier's status capsule: a dot and a word of one tone on a quiet fill of
/// the same tone, for the exceptions only (a failing check, a blocker).
pub(crate) fn status_capsule(tone: Hsla, label: impl Into<SharedString>, cx: &App) -> Div {
    h_flex()
        .h(px(CAPSULE_HEIGHT))
        .px(px(CAPSULE_PAD_X))
        .gap(px(CAPSULE_GAP))
        .items_center()
        .flex_shrink_0()
        .rounded(px(CAPSULE_RADIUS))
        .bg(crate::skin::capsule_bg(tone, cx))
        .child(
            div()
                .size(px(CAPSULE_DOT))
                .rounded_full()
                .flex_shrink_0()
                .bg(tone),
        )
        .child(div().text_color(tone).child(label.into()))
}

/// Glacier's count capsule on a section header, in the section's tone.
fn count_capsule(tone: Hsla, count: usize, cx: &App) -> Div {
    h_flex()
        .h(px(CAPSULE_HEIGHT))
        .px(px(CAPSULE_PAD_X))
        .items_center()
        .flex_shrink_0()
        .rounded(px(CAPSULE_RADIUS))
        .bg(crate::skin::capsule_bg(tone, cx))
        .text_size(type_size::CAPTION)
        .font_weight(FontWeight::MEDIUM)
        .text_color(tone)
        .child(count.to_string())
}

/// One function per cell, each drawn for one PR row of `rows[at]`.
impl BoardTableDelegate {
    fn pr_cell(
        &mut self,
        at: usize,
        row_ix: usize,
        _col_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> AnyElement {
        let row = &self.rows[at];
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        // Draft rows dim their text — inactive, not merely different.
        let dim = row.draft;
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
                    crate::open::open_web_url(cx, &url);
                }
            }));
        let mut cell = h_flex()
            .relative()
            .w_full()
            .h(px(20.))
            .pr(px(72.))
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
                    .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx)),
            );
        }
        cell = cell.child(number);
        if let Some(handler) = self.on_row_action.clone() {
            let vscode_handler = handler.clone();
            let vscode_pr_id = row.id.clone();
            cell = cell.child(
                div()
                    .id(("vscode-pr", row_ix))
                    .absolute()
                    .right(px(24.))
                    .top_0()
                    .size(px(20.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .text_color(theme.muted_foreground)
                    .hover(|style| style.bg(theme.muted).text_color(theme.foreground))
                    .aria_label(format!("Open #{} in VS Code", row.number))
                    .child(
                        gpui::svg()
                            .path("icons/vscode.svg")
                            .size(px(15.))
                            .text_color(theme.muted_foreground),
                    )
                    .tooltip(|window, cx| Tooltip::new("Open in VS Code").build(window, cx))
                    .on_mouse_down(MouseButton::Left, move |event, window, cx| {
                        cx.stop_propagation();
                        if event.click_count == 1 {
                            vscode_handler(vscode_pr_id.clone(), RowAction::OpenVscode, window, cx);
                        }
                    })
                    .on_click(|_, _, cx| cx.stop_propagation()),
            );
            let agents_handler = handler.clone();
            let agents_pr_id = row.id.clone();
            cell = cell.child(
                div()
                    .id(("agents-pr", row_ix))
                    .absolute()
                    .right_0()
                    .top_0()
                    .size(px(20.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .text_color(theme.muted_foreground)
                    .hover(|style| style.bg(theme.muted).text_color(theme.foreground))
                    .aria_label(format!("Review #{} in VS Code Agents", row.number))
                    .child(
                        gpui::svg()
                            .path("icons/blobby.svg")
                            .size(px(16.))
                            .text_color(theme.muted_foreground),
                    )
                    .tooltip(|window, cx| {
                        Tooltip::new("Review in VS Code Agents").build(window, cx)
                    })
                    .on_mouse_down(MouseButton::Left, move |event, window, cx| {
                        cx.stop_propagation();
                        if event.click_count == 1 {
                            agents_handler(
                                agents_pr_id.clone(),
                                RowAction::ReviewVscode,
                                window,
                                cx,
                            );
                        }
                    })
                    .on_click(|_, _, cx| cx.stop_propagation()),
            );
            let watched = self.watched.contains(&row.id);
            // A watched PR shows its binoculars always; an unwatched one
            // only while its row is hovered or selected, at the muted ink's
            // full strength (critique 2026-10-21: twelve grey glyphs at
            // 0.55 opacity measured 2.3:1 and were the board's main noise).
            // The slot keeps its width either way, so the number never
            // moves.
            let revealed = watched || self.caret == Some(row_ix) || self.multi.contains(&row.id);
            let ink = if watched {
                theme.foreground
            } else {
                theme.muted_foreground
            };
            let pr_id = row.id.clone();
            cell = cell.child(
                div()
                    .id(("watch-pr", row_ix))
                    .absolute()
                    .right(px(48.))
                    .top_0()
                    .flex_shrink_0()
                    .size(px(20.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .when(!revealed, |slot| {
                        slot.invisible()
                            .group_hover(ROW_GROUP, |style| style.visible())
                    })
                    .text_color(ink)
                    .hover(|style| style.bg(theme.muted).text_color(theme.foreground))
                    .aria_label(if watched {
                        format!("Unwatch #{}", row.number)
                    } else {
                        format!("Watch #{}", row.number)
                    })
                    .child(
                        gpui::svg()
                            .path("icons/binoculars.svg")
                            .size(px(15.))
                            .text_color(ink),
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
                            handler(pr_id.clone(), RowAction::Watch, window, cx);
                        }
                    })
                    .on_click(|_, _, cx| cx.stop_propagation()),
            );
        }
        cell.into_any_element()
    }

    fn labels_cell(
        &mut self,
        at: usize,
        row_ix: usize,
        col_ix: usize,
        window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> AnyElement {
        let row = &self.rows[at];
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let cell: Div = {
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
                // "bug" must never hide behind the +n overflow, and in
                // All open a label nearly every row carries goes last.
                let ordered = label_order(&row.labels, &self.common_labels);
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
                let avail = f32::from(self.columns[col_ix].width) - CELL_PADDING - ELIDE_SAFETY;
                self.fits.check(window);
                let (visible, hidden) = match self.fits.chips.get(&at) {
                    Some((fitted, fit)) if *fitted == avail => fit.clone(),
                    _ => {
                        let fit = fit_label_chips(window, &texts, avail);
                        self.fits.chips.insert(at, (avail, fit.clone()));
                        fit
                    }
                };
                let mut chips = h_flex().gap_1().flex_shrink_0();
                let (hover_border, hover_text) = (muted, theme.foreground);
                for (ix, text) in visible {
                    let label = &ordered[ix];
                    let chip = label_chip(theme, crate::skin::glacier(cx), cx)
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
                                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
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
                // "+n" sits outside the chips' tooltip, which would
                // otherwise open over the menu it drops down.
                let mut cell = h_flex().gap_1().overflow_hidden();
                let tip: SharedString = if self.on_filter_click.is_some() {
                    format!("{full}\nClick a label to search for label:<name>").into()
                } else {
                    full.into()
                };
                cell = cell.child(
                    chips
                        .id(("labels", row_ix))
                        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx)),
                );
                if hidden > 0 {
                    let more = format!("+{hidden}");
                    cell = cell.child(match self.on_filter_click.clone() {
                        // "+n" opens the hidden labels, each a filter.
                        Some(handler) => {
                            let rest = ordered[ordered.len() - hidden..].to_vec();
                            div()
                                .flex_shrink_0()
                                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                .child(
                                    Button::new(("more-labels", row_ix))
                                        .ghost()
                                        .xsmall()
                                        .h(px(CHIP_HEIGHT))
                                        .px(px(CHIP_PAD_X))
                                        .rounded(px(CHIP_RADIUS))
                                        .map(|chip| {
                                            if crate::skin::glacier(cx) {
                                                chip.bg(crate::skin::chip_bg(cx))
                                                    .border_1()
                                                    .border_color(crate::skin::chip_edge(cx))
                                            } else {
                                                chip.bg(theme.muted)
                                                    .border_1()
                                                    .border_color(theme.border)
                                            }
                                        })
                                        .child(
                                            // A Button label ignores the
                                            // button's text size.
                                            div()
                                                .text_size(CHIP_TEXT)
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
                        None => label_chip(theme, crate::skin::glacier(cx), cx)
                            .flex_shrink_0()
                            .text_color(muted)
                            .child(more)
                            .into_any_element(),
                    });
                }
                return cell.into_any_element();
            }
        };
        cell.into_any_element()
    }

    // The calm rule (spec §8): bad states get colored text, good
    // states get only a colored dot with muted text.
    fn ci_cell(
        &mut self,
        at: usize,
        _row_ix: usize,
        _col_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> AnyElement {
        let row = &self.rows[at];
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let glacier = crate::skin::glacier(cx);
        let cell: Div = match row.ci {
            // Glacier: the one exception in a capsule; pass and running
            // stay dot + word.
            Ci::Fail if glacier => {
                h_flex()
                    .items_center()
                    .child(status_capsule(theme.danger, "fail", cx))
            }
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
        };
        cell.into_any_element()
    }

    fn repo_cell(
        &mut self,
        at: usize,
        row_ix: usize,
        _col_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> AnyElement {
        let row = &self.rows[at];
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let chip = FilterChip::new(Qualifier::Repo, row.repo.as_str());
        div()
            .text_color(muted)
            .child(self.filter_target(("repo-filter", row_ix), row.repo.clone(), Some(chip)))
            .into_any_element()
    }

    fn author_cell(
        &mut self,
        at: usize,
        row_ix: usize,
        _col_ix: usize,
        _window: &mut Window,
        _cx: &mut Context<TableState<Self>>,
    ) -> AnyElement {
        let row = &self.rows[at];
        let chip = row
            .author
            .as_deref()
            .map(|author| FilterChip::new(Qualifier::Author, author));
        let text = row.author.clone().unwrap_or_else(|| "?".into());
        self.filter_target(("author-filter", row_ix), text, chip)
            .into_any_element()
    }

    fn unresolved_cell(
        &mut self,
        at: usize,
        _row_ix: usize,
        _col_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> AnyElement {
        let row = &self.rows[at];
        let theme = cx.theme();
        let color = if row.unresolved > 0 {
            theme.danger
        } else {
            theme.success
        };
        let count = if row.unresolved > 0 && row.unresolved_capped {
            format!("{}+", row.unresolved)
        } else {
            row.unresolved.to_string()
        };
        h_flex()
            .gap_1p5()
            .items_center()
            .child(status_dot(color))
            .child(div().text_color(color).child(count))
            .into_any_element()
    }

    /// Whether the Ticket column is on screen, so the title leaves the issue
    /// to it.
    fn shows_ticket(&self) -> bool {
        self.kinds.contains(&Some(ColumnKind::Ticket))
    }

    /// The linked issue, a link to the tracker (a Linear ticket, say) when
    /// the issue rule has a URL; a dash without one.
    fn ticket_cell(
        &mut self,
        at: usize,
        row_ix: usize,
        _col_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> AnyElement {
        let row = &self.rows[at];
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let Some(issue) = row.issue.clone() else {
            return h_flex()
                .child(div().text_color(muted.opacity(0.5)).child("—"))
                .into_any_element();
        };
        let Some(url) = row.issue_url.clone() else {
            return h_flex()
                .child(div().text_color(muted).child(issue))
                .into_any_element();
        };
        // Linear's comment names the issue; the hover says which it is.
        let tip = match &row.issue_title {
            Some(title) => format!("{issue} · {title}"),
            None => format!("Open {issue}"),
        };
        h_flex()
            .child(
                div()
                    .id(("ticket-link", row_ix))
                    .cursor_pointer()
                    .text_color(if row.draft { muted } else { theme.link })
                    .hover(|style| style.underline())
                    .child(issue)
                    .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |_, e: &ClickEvent, _, cx| {
                        cx.stop_propagation();
                        if e.click_count() == 1 {
                            crate::open::open_web_url(cx, &url);
                        }
                    })),
            )
            .into_any_element()
    }

    /// Lines added in green and removed in red, as GitHub shows them; the
    /// changed files on hover.
    fn lines_cell(
        &mut self,
        at: usize,
        row_ix: usize,
        _col_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> AnyElement {
        let theme = cx.theme();
        let Some(size) = self.rows[at].size else {
            return h_flex()
                .child(
                    div()
                        .text_color(theme.muted_foreground.opacity(0.5))
                        .child("—"),
                )
                .into_any_element();
        };
        let files = match size.changed_files {
            1 => "1 file changed".to_owned(),
            n => format!("{n} files changed"),
        };
        let tip = format!(
            "{} added, {} removed · {files}",
            size.additions, size.deletions
        );
        h_flex()
            .id(("lines", row_ix))
            .gap_1p5()
            .items_center()
            .child(
                div()
                    .text_color(theme.success)
                    .child(format!("+{}", short_count(size.additions))),
            )
            .child(
                div()
                    .text_color(theme.danger)
                    .child(format!("−{}", short_count(size.deletions))),
            )
            .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
            .into_any_element()
    }

    fn review_column_cell(
        &mut self,
        at: usize,
        row_ix: usize,
        col_ix: usize,
        window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> AnyElement {
        let row = &self.rows[at];
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let cell: Div = {
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
                        .tooltip(move |window, cx| Tooltip::new(hover.clone()).build(window, cx))
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
                        - px(CELL_PADDING + GAP_1 + GAP_1 + ELIDE_SAFETY);
                    let names =
                        self.fits
                            .elide(window, (at, CellPart::ReviewNames), &names, avail, None);
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
        };
        cell.into_any_element()
    }

    fn title_cell(
        &mut self,
        at: usize,
        row_ix: usize,
        col_ix: usize,
        window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> AnyElement {
        let row = &self.rows[at];
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        // Draft rows dim their text — inactive, not merely different.
        let dim = row.draft;
        // The Ticket column, when shown, carries the issue instead.
        let issue = row.issue.as_ref().filter(|_| !self.shows_ticket());
        let mut full = match issue {
            Some(issue) => format!("{issue} · {}", row.title),
            None => row.title.clone(),
        };
        let stack_prefix = row.stack.as_ref().map(|s| {
            full.push('\n');
            full.push_str(&prmarmot_core::cells::stack_hover(s));
            prmarmot_core::cells::stack_prefix(s, self.stack_ends_at(row_ix))
        });
        // Glacier sets the title a step bolder, and measures it so.
        let title_weight = crate::skin::glacier(cx).then_some(FontWeight::MEDIUM);
        let stack_w = stack_prefix
            .as_deref()
            .map(|s| measure_width_weighted(window, s, title_weight) + px(GAP_1))
            .unwrap_or(px(0.));
        let tag_color = if dim { muted } else { theme.accent_foreground };
        // Elide the title to the width the flexible region actually has
        // (column minus the issue tag, gap and cell padding). A real
        // "…", not a mid-word clip — the affordance that says "there's
        // more, hover" (critique #3). Done by hand via `elide` because
        // gpui's in-cell `.truncate()` is inert in this table.
        let col_w = self.columns[col_ix].width;
        let issue_w = issue
            .map(|s| measure_width_weighted(window, s, title_weight))
            .unwrap_or(px(0.));
        let gap = if issue.is_some() { GAP_1 } else { 0.0 };
        let avail = col_w - issue_w - stack_w - px(CELL_PADDING + gap + ELIDE_SAFETY);
        let title = self.fits.elide(
            window,
            (at, CellPart::Title),
            &row.title,
            avail,
            title_weight,
        );
        let inner = match (issue, &row.issue_url) {
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
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(cx.listener(move |_, e: &ClickEvent, _, cx| {
                                if e.click_count() == 1 {
                                    crate::open::open_web_url(cx, &issue_url);
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
        h_flex()
            .w_full()
            .gap_1()
            .overflow_hidden()
            .when_some(title_weight, |this, weight| this.font_weight(weight))
            .when_some(stack_prefix, |this, prefix| {
                this.child(div().flex_shrink_0().text_color(muted).child(prefix))
            })
            .child(inner)
            .id(("title", row_ix))
            .tooltip(move |window, cx| Tooltip::new(full.clone()).build(window, cx))
            .into_any_element()
    }

    fn note_cell(
        &mut self,
        at: usize,
        row_ix: usize,
        col_ix: usize,
        window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> AnyElement {
        let row = &self.rows[at];
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        // Exception-first Note (note-hierarchy plan): the dot + a single
        // emphasized primary phrase carry the row's worst blocker;
        // every other blocker trails as muted context so nothing hides
        // in the tooltip. Only genuinely exceptional blockers get red —
        // routine "assign reviewers" / "resolve N" rows are amber-muted,
        // so a column of them no longer reads as one red wall.
        let NoteParts {
            tone,
            primary,
            tail,
            wait,
            size,
            tooltip,
        } = self.note_parts(row);
        let (dot_color, primary_color) = note_tone_colors(tone, theme);
        // Glacier: the danger tone alone sits in a capsule of its ink — on a
        // real board almost every Needs-action row is amber, and a column of
        // amber capsules let nothing stand out (Oliver, 2026-10-20). A
        // warning stays Classic's dot + words.
        let capsule = crate::skin::glacier(cx) && tone == Tone::Danger;
        let primary_color = if capsule {
            tone_color(tone, theme)
        } else {
            primary_color
        };
        // pr_2: the terminal column needs an optical margin the 6px
        // cell pad doesn't give (critique #4).
        let mut cell = h_flex()
            .w_full()
            .gap_1p5()
            .items_center()
            .overflow_hidden()
            .pr_2();
        if let Some(color) = dot_color.filter(|_| !capsule) {
            cell = cell.child(status_dot(color));
        }
        // The muted tail absorbs the ellipsis when the row is narrow;
        // the wait and size never elide. The primary is shortened only
        // when it cannot fit beside them on its own: a calm note is
        // one whole sentence, and clipped it would hide the wait.
        // (A real "…", see `elide`.)
        let dot_region = if capsule {
            2. * CAPSULE_PAD_X + CAPSULE_DOT + CAPSULE_GAP
        } else if dot_color.is_some() {
            STATUS_DOT + GAP_1P5
        } else {
            0.0
        };
        let wait_w = wait
            .as_ref()
            .map_or(px(0.), |(label, _)| measure_width(window, label));
        let size_w = size
            .as_ref()
            .map_or(px(0.), |label| measure_width(window, label));
        let room = self.columns[col_ix].width
            - wait_w
            - size_w
            - px(CELL_PADDING + 8.0 + dot_region + GAP_1P5 * 3. + ELIDE_SAFETY);
        let mut primary_w = measure_width(window, &primary);
        let primary = if primary_w > room {
            let shortened =
                self.fits
                    .elide(window, (at, CellPart::NotePrimary), &primary, room, None);
            primary_w = measure_width(window, &shortened);
            shortened
        } else {
            primary
        };
        cell = cell.child(if capsule {
            status_capsule(primary_color, primary, cx)
        } else {
            div()
                .flex_shrink_0()
                .text_color(primary_color)
                .child(primary)
        });
        let tail_room = room - primary_w;
        // Below a few characters' room the tail would be a bare "…".
        if !tail.is_empty() && tail_room >= px(TAIL_MIN) {
            let tail = self
                .fits
                .elide(window, (at, CellPart::NoteTail), &tail, tail_room, None);
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
        cell.id(("note", row_ix))
            .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
            .into_any_element()
    }
}

/// The Note cell's text, composed once for drawing and for measuring.
struct NoteParts {
    tone: Tone,
    primary: String,
    /// The muted tail: the primary's remedy, then the remaining blockers
    /// as context, in presentation-priority order.
    tail: String,
    /// How long it has waited for a reviewer, and whether that is stale.
    wait: Option<(String, bool)>,
    /// The size band, in the views that pick a review.
    size: Option<String>,
    tooltip: String,
}

/// The widest Title and Note content on the board, in px, as the cells draw
/// them: Glacier fits the two elastic columns to it (`fit_to_content`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ContentNeed {
    pub title: f32,
    pub note: f32,
    /// The widest Note's dot or capsule and first phrase alone: what a Note
    /// column must hold whole, whatever Title takes.
    pub note_primary: f32,
    /// The widest title: Title may grow past its claim up to this, but only
    /// into room Note's claim does not use.
    pub title_max: f32,
    /// Note's claim, capped like Title's (`title_claim`): what 85 % of the
    /// Notes fit in; one unusually long Note elides rather than hold Title.
    pub note_claim: f32,
}

impl BoardTableDelegate {
    fn note_parts(&self, row: &BoardRow) -> NoteParts {
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
        // In the review queue and All open the size band trails
        // last, muted: both are lists to pick a review from.
        let size = row
            .size
            .filter(|_| matches!(self.mode, Mode::Review | Mode::AllOpen));
        if let Some(size) = size {
            tooltip.push_str(&format!("\nSize: {}", size_text(size)));
        }
        let size = size.map(|size| format!(" · {}", size.band().label()));
        let mut tail = String::new();
        if let Some(remedy) = &remedy {
            tail.push_str(" — ");
            tail.push_str(remedy);
        }
        for fact in &context {
            tail.push_str(" · ");
            tail.push_str(fact);
        }
        NoteParts {
            tone,
            primary,
            tail,
            wait,
            size,
            tooltip,
        }
    }

    /// A number that changes whenever the shown rows do, so a measurement of
    /// them can be reused until then.
    pub fn display_version(&self) -> u64 {
        self.display_version
    }

    /// The widest Title and Note on the board, measured as `title_cell` and
    /// `note_cell` draw them (the same weight, tag, prefix, dot or capsule,
    /// tail, wait and size), over the rows on screen.
    pub fn content_need(&self, window: &mut Window, cx: &App) -> ContentNeed {
        let theme = cx.theme();
        let glacier = crate::skin::glacier(cx);
        let title_weight = glacier.then_some(FontWeight::MEDIUM);
        let mut need = ContentNeed::default();
        let mut titles = Vec::new();
        let mut notes = Vec::new();
        for display_ix in 0..self.display.len() {
            let Some(row) = self.row(display_ix) else {
                continue;
            };
            let stack_w = row
                .stack
                .as_ref()
                .map(|s| prmarmot_core::cells::stack_prefix(s, self.stack_ends_at(display_ix)))
                .map(|prefix| {
                    f32::from(measure_width_weighted(window, &prefix, title_weight)) + GAP_1
                })
                .unwrap_or(0.0);
            let issue_w = row
                .issue
                .as_deref()
                .filter(|_| !self.shows_ticket())
                .map(|s| f32::from(measure_width_weighted(window, s, title_weight)) + GAP_1)
                .unwrap_or(0.0);
            let title_w = f32::from(measure_width_weighted(window, &row.title, title_weight));
            titles.push(issue_w + stack_w + title_w + CELL_PADDING + ELIDE_SAFETY);

            let parts = self.note_parts(row);
            let (dot_color, _) = note_tone_colors(parts.tone, theme);
            let dot_region = if glacier && parts.tone == Tone::Danger {
                2. * CAPSULE_PAD_X + CAPSULE_DOT + CAPSULE_GAP
            } else if dot_color.is_some() {
                STATUS_DOT + GAP_1P5
            } else {
                0.0
            };
            let primary_w = f32::from(measure_width(window, &parts.primary));
            // The phrase Note never gives up (`note_floor_phrase`); a calm
            // note's whole sentence can be as wide as a title.
            let head_w = f32::from(measure_width(window, note_floor_phrase(&parts.primary)));
            let tail_w = f32::from(measure_width(window, &parts.tail));
            let wait_w = match &parts.wait {
                Some((label, _)) => f32::from(measure_width(window, label)),
                None => 0.0,
            };
            let size_w = match &parts.size {
                Some(label) => f32::from(measure_width(window, label)),
                None => 0.0,
            };
            let fixed = CELL_PADDING + 8.0 + dot_region + ELIDE_SAFETY;
            let note_w = primary_w + tail_w + wait_w + size_w + fixed + GAP_1P5 * 3.;
            need.note = need.note.max(note_w);
            notes.push(note_w);
            need.note_primary = need.note_primary.max(head_w + fixed);
        }
        need.title_max = titles.iter().copied().fold(0.0, f32::max);
        need.title = title_claim(&mut titles);
        need.note_claim = title_claim(&mut notes);
        need
    }
}

/// The width a column claims for a set of rows: the 85th percentile of its
/// cells' widths, so one unusually long title (or Note) does not widen the
/// column for every row (it elides, its tooltip has the whole text); with
/// fewer than seven rows, the widest (Oliver, 2026-10-21: "Title first,
/// capped"). Title and Note both claim this way.
fn title_claim(widths: &mut [f32]) -> f32 {
    if widths.is_empty() {
        return 0.0;
    }
    widths.sort_by(f32::total_cmp);
    if widths.len() < 7 {
        return widths[widths.len() - 1];
    }
    // Nearest rank: the smallest width at or above which 85 % of rows fall.
    let rank = (0.85 * widths.len() as f32).ceil() as usize;
    widths[rank.clamp(1, widths.len()) - 1]
}

/// The part of a Note's primary phrase the column must hold whole: its head
/// (the words before the first " · ") and, when that head is an owner
/// prefix ("X's PR"), the first action phrase after it too, so a note still
/// says "josefrichter's PR · merge conflict" and not just whose PR it is.
fn note_floor_phrase(primary: &str) -> &str {
    let mut segments = primary.split(" · ");
    let head = segments.next().unwrap_or_default();
    match segments.next() {
        Some(action) if head.ends_with("'s PR") => {
            &primary[..head.len() + " · ".len() + action.len()]
        }
        _ => head,
    }
}

/// Glacier: fit Title and Note to what they hold, Title first (Oliver,
/// 2026-10-21). Title gets its claim (`title_claim`: what fits 85 % of the
/// rows' titles); Note takes everything Title does not need and its tail
/// elides. What Note never gives up is the floor phrase of its primary
/// (`note_floor_phrase`, with its dot or capsule), so when that would not
/// fit, Title gives room back — down to its responsive split, never past
/// it. Note also takes the scrollbar allowance the responsive split leaves
/// after it, so no empty cell trails the table.
/// Classic keeps the split, where Note wins (DESIGN.md). Returns whether a
/// width changed.
pub fn fit_to_content(cols: &mut [Column], need: ContentNeed, all_repos: bool) -> bool {
    let kind_ix = |kind: ColumnKind| cols.iter().position(|c| ColumnKind::of(c) == Some(kind));
    let (Some(title_ix), Some(note_ix)) = (kind_ix(ColumnKind::Title), kind_ix(ColumnKind::Note))
    else {
        return false;
    };
    let split_title = f32::from(cols[title_ix].width);
    let budget = split_title + f32::from(cols[note_ix].width) + SCROLLBAR_MARGIN;
    let title_min = if all_repos { 120.0 } else { TITLE_MIN };
    let quantized = |w: f32| (w / QUANTUM).ceil() * QUANTUM;
    let title_need = quantized(need.title).max(title_min);
    // Note's floor: its widest floor phrase whole, and never under what
    // Title's own floor leaves.
    let note_floor = quantized(need.note_primary).min(budget - title_min);
    // Title shrinks for Note's floor only as far as its responsive split.
    let title_keep = split_title.max(title_min).min(title_need);
    let mut title_w = title_need.min(budget - note_floor).max(title_keep);
    // Room Note's claim leaves is Title's, up to its widest title: a title
    // elides for no one when most Notes have nothing to put there.
    let spare = budget - title_w - quantized(need.note_claim);
    if spare > 0.0 {
        title_w = (title_w + spare).min(quantized(need.title_max).max(title_w));
    }
    let note_w = budget - title_w;
    let changed = cols[title_ix].width != px(title_w) || cols[note_ix].width != px(note_w);
    cols[title_ix].width = px(title_w);
    cols[note_ix].width = px(note_w);
    changed
}

impl TableDelegate for BoardTableDelegate {
    /// A header dragged to a new place; `RootView` saves the order from
    /// `TableEvent::MoveColumn`.
    fn move_column(
        &mut self,
        col_ix: usize,
        to_ix: usize,
        _window: &mut Window,
        _cx: &mut Context<TableState<Self>>,
    ) {
        if col_ix >= self.columns.len() || to_ix >= self.columns.len() {
            return;
        }
        let column = self.columns.remove(col_ix);
        self.columns.insert(to_ix, column);
        let kind = self.kinds.remove(col_ix);
        self.kinds.insert(to_ix, kind);
        self.fits.clear();
    }

    fn render_th(
        &mut self,
        col_ix: usize,
        _window: &mut Window,
        _cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let visible = self.kinds.clone();
        let handler = self.on_column_toggle.clone();
        div()
            .id(("column-menu", col_ix))
            .size_full()
            .child(self.columns[col_ix].name.clone())
            .context_menu(move |mut menu, _, _| {
                for kind in ColumnKind::ALL {
                    let checked = visible.contains(&Some(kind));
                    let handler = handler.clone();
                    menu = menu.item(
                        PopupMenuItem::new(kind.header())
                            .checked(checked)
                            .disabled(checked && visible.len() == 1)
                            .on_click(move |_, window, cx| {
                                if let Some(handler) = &handler {
                                    handler(kind, window, cx);
                                }
                            }),
                    );
                }
                menu
            })
    }

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
        if self.multi.contains(&row.id) {
            return self.selection_menu(menu);
        }
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
            actions.push((
                crate::attention_state::SNOOZE_CANCEL,
                RowAction::CancelSnooze,
            ));
        }
        for (index, (label, action)) in actions.into_iter().enumerate() {
            if index == 2 || index == 7 {
                menu = menu.separator();
            }
            let pr_id = row.id.clone();
            let handler = handler.clone();
            menu = menu.item(PopupMenuItem::new(label).on_click(move |_, window, cx| {
                handler(pr_id.clone(), action.clone(), window, cx);
            }));
        }
        let pr_id = row.id.clone();
        menu.separator()
            .item(
                PopupMenuItem::new("Refresh Linear ticket").on_click(move |_, window, cx| {
                    handler(pr_id.clone(), RowAction::RefreshTicket, window, cx);
                }),
            )
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
        window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> Stateful<Div> {
        let tr = div().id(("board-row", row_ix));
        let theme = cx.theme();
        let glacier = crate::skin::glacier(cx);
        let window_active = window.is_window_active();
        match self.display.get(row_ix) {
            // Section header: a subtle band with a stronger top rule, and the
            // label drawn as an absolute overlay (cells render empty on this
            // row so nothing paints over it — see the module note).
            Some(DisplayRow::Header {
                kind,
                collapsed,
                label,
                count,
                detail,
                explanation,
                members,
            }) => {
                let section_tone = prmarmot_core::cells::section_tone(kind);
                // The calm rule on a section that is fine: Approved's rail,
                // band and count are muted, not green (critique D2); danger
                // and warning stay saturated. My PR approved is yours to
                // merge, so its header is green the way Awaiting their
                // review's is amber.
                let header_tone =
                    if section_tone == Tone::Success && *kind != SectionKind::MyApproved {
                        theme.muted_foreground
                    } else {
                        tone_color(section_tone, theme)
                    };
                tr.relative()
                    .group(HEADER_GROUP)
                    .aria_label(format!("{label} section"))
                    .bg(theme.background)
                    // Classic parts a section from the rows above with its
                    // hairline; Glacier's sheet has the band for that.
                    .when(count.is_some() && !glacier, |header| {
                        header.border_t_1().border_color(theme.border)
                    })
                    // The band — of the section's tone, or the table's own
                    // colour on a stack sub-header — is painted as a child
                    // over the row, so the table's row hover never shows on
                    // a header, which can't be selected (critique C4); then
                    // a rail at the table's edge, inset and rounded so a
                    // header scrolled under the column labels leaves no
                    // sliver of it (R1). Both skins since the 2026-10-21
                    // critique: Classic's uniform grey bands made Needs
                    // action look like Drafts.
                    .map(|header| {
                        let band = if count.is_some() {
                            crate::skin::band_bg(header_tone, cx)
                        } else {
                            theme.background
                        };
                        header.child(div().absolute().inset_0().bg(band)).when(
                            count.is_some(),
                            |header| {
                                header.child(
                                    div()
                                        .absolute()
                                        .left_0()
                                        .top(px(RAIL_INSET))
                                        .bottom(px(RAIL_INSET))
                                        .w(px(RAIL))
                                        .rounded_r(px(RAIL / 2.))
                                        .bg(header_tone),
                                )
                            },
                        )
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
                                h_flex()
                                    .id(("section-toggle", row_ix))
                                    .items_center()
                                    .gap_1p5()
                                    // A top-level header is its section's
                                    // disclosure: the chevron swaps glyphs, nothing
                                    // animates.
                                    .when_some(
                                        count.and(self.on_section_toggle.clone()),
                                        |toggle, handler| {
                                            let kind = *kind;
                                            toggle
                                                .cursor_pointer()
                                                .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                                    cx.stop_propagation()
                                                })
                                                .on_click(move |_, window, cx| {
                                                    cx.stop_propagation();
                                                    handler(kind, window, cx);
                                                })
                                                .child(
                                                    // The glyph draws well under its
                                                    // font size: a larger size on a
                                                    // fixed line keeps the row height.
                                                    div()
                                                        .w(px(14.))
                                                        .text_size(px(17.))
                                                        .line_height(px(14.))
                                                        .text_color(theme.secondary_foreground)
                                                        .child(if *collapsed {
                                                            "▸"
                                                        } else {
                                                            "▾"
                                                        }),
                                                )
                                        },
                                    )
                                    .child(
                                        div()
                                            .id(("section-title", row_ix))
                                            .text_size(match (count.is_some(), glacier) {
                                                (true, true) => type_size::BODY,
                                                (true, false) | (false, true) => type_size::SMALL,
                                                (false, false) => type_size::CAPTION,
                                            })
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(match (count.is_some(), glacier) {
                                                (true, true) => theme.foreground,
                                                (false, true) => theme.muted_foreground,
                                                (_, false) => theme.secondary_foreground,
                                            })
                                            .child(label.clone())
                                            // What puts a PR here, on hover over the title
                                            // only, so it never covers the Copy menu. The text
                                            // is capped so it wraps rather than runs off a
                                            // narrow window (a cap on the tooltip box doesn't wrap).
                                            .when_some(explanation.clone(), |title, text| {
                                                title.tooltip(move |window, cx| {
                                                    let text = text.clone();
                                                    Tooltip::element(move |_, _| {
                                                        div().max_w(px(460.)).child(text.clone())
                                                    })
                                                    .build(window, cx)
                                                })
                                            }),
                                    )
                                    .when_some(*count, |header, count| {
                                        header.child(if glacier {
                                            count_capsule(header_tone, count, cx)
                                        } else {
                                            div()
                                                .px(px(6.))
                                                .rounded(px(4.))
                                                .bg(theme.background)
                                                .text_size(type_size::CAPTION)
                                                .font_weight(FontWeight::MEDIUM)
                                                .text_color(theme.muted_foreground)
                                                .child(count.to_string())
                                        })
                                    })
                                    .when_some(detail.clone(), |header, detail| {
                                        // Folded, the summary is the section's
                                        // content: read, not greyed out.
                                        header.child(
                                            div()
                                                .text_size(type_size::CAPTION)
                                                .text_color(if *collapsed {
                                                    theme.secondary_foreground
                                                } else {
                                                    theme.muted_foreground
                                                })
                                                .child(format!("· {detail}")),
                                        )
                                    }),
                            )
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
                                                    group_copy_menu(
                                                        menu,
                                                        table.clone(),
                                                        label.clone(),
                                                    )
                                                })
                                                .on_open_change(move |open, window, _| {
                                                    open_state.set(open.then_some(row_ix));
                                                    window.refresh();
                                                }),
                                        ),
                                )
                            }),
                    )
            }
            // No per-row tint: the "Needs action" section header, the red note
            // dot, and the red problem phrase already signal urgency three
            // times over. A red row band on top of that dominates the screen
            // and flattens individual CI-fail / conflict rows into one alarm
            // block (design review, 2026-07-24). Zebra striping stays; state
            // lives in the Note cell.
            Some(DisplayRow::Pr(ix)) => {
                let selected = self
                    .rows
                    .get(*ix)
                    .is_some_and(|row| self.multi.contains(&row.id));
                let intent = self.click_intent.clone();
                let caret = glacier && self.caret == Some(row_ix);
                let number = self.rows.get(*ix).map(|row| row.number).unwrap_or(0);
                tr.group(ROW_GROUP)
                    .aria_label(format!("Pull request #{number}"))
                    .when(glacier, |row| row.relative())
                    // Built-in striping generates filler rows below the data.
                    // Style real PRs here instead; headers keep their own bands.
                    // Glacier parts its rows with a rule, not zebra.
                    .when(ix % 2 != 0 && !glacier, |row| row.bg(theme.table_even))
                    // Part of a multi-selection: the caret row's tint without
                    // its border, which the framework draws on the caret.
                    // Glacier paints it as a child over the row, so hovering
                    // a selected row keeps the selection fill (critique C3).
                    .when(selected && !glacier, |row| row.bg(theme.table_active))
                    .when(selected && glacier, |row| {
                        row.child(div().absolute().inset_0().bg(if window_active {
                            theme.table_active
                        } else {
                            theme.table_hover
                        }))
                    })
                    // Glacier: the selection is a rail at the sheet's edge in
                    // the link colour, with an outline on the caret row; in an
                    // inactive window the rail is muted and there is no outline.
                    .when(glacier && (caret || selected), |row| {
                        let edge = if window_active {
                            theme.link
                        } else {
                            theme.muted_foreground
                        };
                        row.child(
                            div()
                                .absolute()
                                .left_0()
                                .top_0()
                                .bottom_0()
                                .w(px(RAIL))
                                .bg(edge),
                        )
                        .when(caret && window_active, |row| {
                            row.child(
                                div()
                                    .absolute()
                                    .top_0()
                                    .left_0()
                                    .right_0()
                                    .bottom(px(-1.))
                                    .border_1()
                                    .border_color(edge),
                            )
                        })
                    })
                    .when(self.stack_ends_at(row_ix), |row| {
                        row.border_b_1().border_color(theme.border)
                    })
                    // The click's modifiers, for the `SelectRow` it becomes.
                    .on_mouse_down(MouseButton::Left, move |event, _, _| {
                        intent.set(Some((row_ix, ClickIntent::from_modifiers(event.modifiers))));
                    })
            }
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
        let at = match self.display.get(row_ix) {
            Some(&DisplayRow::Pr(i)) if i < self.rows.len() => i,
            _ => return div().into_any_element(),
        };

        let Some(kind) = self.kinds.get(col_ix).copied().flatten() else {
            return div().into_any_element();
        };
        match kind {
            ColumnKind::Pr => self.pr_cell(at, row_ix, col_ix, window, cx),
            ColumnKind::Labels => self.labels_cell(at, row_ix, col_ix, window, cx),
            ColumnKind::Ci => self.ci_cell(at, row_ix, col_ix, window, cx),
            ColumnKind::Repo => self.repo_cell(at, row_ix, col_ix, window, cx),
            ColumnKind::Ticket => self.ticket_cell(at, row_ix, col_ix, window, cx),
            ColumnKind::Author => self.author_cell(at, row_ix, col_ix, window, cx),
            ColumnKind::Unresolved => self.unresolved_cell(at, row_ix, col_ix, window, cx),
            ColumnKind::Lines => self.lines_cell(at, row_ix, col_ix, window, cx),
            ColumnKind::Review => self.review_column_cell(at, row_ix, col_ix, window, cx),
            ColumnKind::Title => self.title_cell(at, row_ix, col_ix, window, cx),
            ColumnKind::Note => self.note_cell(at, row_ix, col_ix, window, cx),
        }
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
        let empty =
            prmarmot_core::status::queue_empty(self.mode, self.all_repos, self.can_load_more);
        let handler = self.on_empty_action.clone();
        // The marmot keeps watch over a board with nothing on it, only when
        // the queue is whole: not when another page could hold something
        // (an action is offered then), and the search's own "no match"
        // body is drawn elsewhere.
        let whole = empty.action.is_none();
        gpui_component::v_flex()
            .size_full()
            .gap_3()
            .items_center()
            .justify_center()
            .text_color(cx.theme().muted_foreground)
            .when(whole, |body| {
                body.child(
                    gpui::img("branding/mascot.png")
                        .w(px(58.))
                        .h(px(64.))
                        .opacity(0.85),
                )
            })
            .child(empty.text)
            .when_some(empty.action.zip(handler), |body, (action, handler)| {
                body.child(
                    Button::new("empty-action")
                        .small()
                        .label(action.label())
                        .on_click(move |_, window, cx| handler(action, window, cx)),
                )
            })
    }
}

#[cfg(test)]
mod tests {

    //! Pure display-model tests — no GPUI context. These guard the section
    //! grouping and the URL-identity selection the design review flagged.
    //! They run under `cargo test` (which compiles the GPUI binary), not the
    //! fast core-only `make check`.
    use super::*;

    #[test]
    fn column_selection_restores_known_keys_and_falls_back_for_empty_selections() {
        let mut original = BoardTableDelegate::new(Mode::Authored, false);
        original.toggle_column(ColumnKind::Title);
        let keys = original.visible_column_keys();
        let mut restored = BoardTableDelegate::new(Mode::Review, true);
        restored.restore_visible_columns(Some(&keys));
        assert_eq!(restored.visible_column_keys(), keys);
        restored.restore_visible_columns(Some(&["obsolete".into(), "pr".into(), "pr".into()]));
        assert_eq!(restored.visible_column_keys(), ["pr"]);
        for keys in [None, Some(vec![]), Some(vec!["obsolete".into()])] {
            restored.restore_visible_columns(keys.as_deref());
            assert!(restored.visible_columns.is_none());
            assert!(!restored
                .customize_columns(columns_for(
                    Mode::Review,
                    TableWidthClass::Compact,
                    800.,
                    false,
                    false
                ))
                .is_empty());
        }
    }

    #[test]
    fn column_selection_survives_queue_and_responsive_changes() {
        let mut delegate = BoardTableDelegate::new(Mode::Authored, false);
        delegate.toggle_column(ColumnKind::Title);
        delegate.toggle_column(ColumnKind::Author);
        delegate.toggle_column(ColumnKind::Repo);
        for mode in [Mode::Authored, Mode::Review, Mode::AllOpen] {
            for class in [TableWidthClass::Compact, TableWidthClass::Wide] {
                let columns =
                    delegate.customize_columns(columns_for(mode, class, 1200., false, false));
                let kinds: Vec<_> = columns.iter().filter_map(ColumnKind::of).collect();
                assert!(!kinds.contains(&ColumnKind::Title));
                assert!(kinds.contains(&ColumnKind::Author));
                assert!(kinds.contains(&ColumnKind::Repo));
            }
        }
    }

    #[test]
    fn column_selection_keeps_a_header_available_and_can_restore_every_column() {
        let mut delegate = BoardTableDelegate::new(Mode::Authored, false);
        for kind in ColumnKind::ALL {
            if delegate.kinds.contains(&Some(kind)) {
                delegate.toggle_column(kind);
                let columns = delegate.customize_columns(columns_for(
                    Mode::Authored,
                    TableWidthClass::Medium,
                    1200.,
                    false,
                    false,
                ));
                delegate.set_columns(columns);
            }
        }
        assert_eq!(delegate.columns.len(), 1);
        for kind in ColumnKind::ALL {
            if !delegate.kinds.contains(&Some(kind)) {
                delegate.toggle_column(kind);
                let columns = delegate.customize_columns(columns_for(
                    Mode::Authored,
                    TableWidthClass::Medium,
                    1200.,
                    false,
                    false,
                ));
                delegate.set_columns(columns);
            }
        }
        assert_eq!(delegate.columns.len(), ColumnKind::ALL.len());
    }

    use prmarmot_core::board::{Blocker, Category, ReviewState};
    use prmarmot_core::layout::group_label;
    use prmarmot_core::search::{matches_filter, matches_search};

    fn rule() -> StaleRule {
        StaleRule {
            now: chrono::DateTime::parse_from_rfc3339("2026-09-15T12:00:00Z")
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
            is_own: true,
            number,
            url: format!("https://github.com/acme/widgets/pull/{number}"),
            title: format!("PR {number}"),
            draft: matches!(category, Category::Draft),
            category,
            ci: Ci::Pass,
            // As the derivation leaves any waiting PR in My PRs: a reviewer
            // is asked. One with nobody asked is Available to review.
            review_state: ReviewState::Waiting,
            ..Default::default()
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
            branch: None,
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
        d.rebuild_display();
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
            vec![SectionKind::Snoozed],
        );
        assert!(delegate
            .display_index_of_url("https://github.com/acme/widgets/pull/1")
            .is_none());
        assert!(delegate
            .display_index_of_url("https://github.com/acme/widgets/pull/2")
            .is_some());
        delegate.collapsed.clear();
        delegate.rebuild_display();
        assert!(delegate
            .display_index_of_url("https://github.com/acme/widgets/pull/1")
            .is_some());
    }

    #[test]
    fn a_folded_header_is_selectable_and_names_its_section() {
        let action = SectionKind::Category(Category::Action);
        let mut delegate = BoardTableDelegate::new(Mode::Authored, false);
        delegate.set_rows(vec![
            row(1, Category::Action),
            row(2, Category::Action),
            row(3, Category::Await),
        ]);
        delegate.set_attention(HashMap::new(), HashSet::new(), HashSet::new(), vec![]);
        // Open: headers are skipped, rows name the section above them.
        let action_ix = delegate.header_index(action).unwrap();
        assert!(!delegate.is_selectable(action_ix));
        assert!(delegate.is_selectable(action_ix + 1));
        assert_eq!(delegate.section_at(action_ix + 2), Some(action));
        assert_eq!(delegate.folded_section_at(action_ix), None);
        // Folded: the header is where the selection can rest.
        delegate.set_attention(HashMap::new(), HashSet::new(), HashSet::new(), vec![action]);
        let action_ix = delegate.header_index(action).unwrap();
        assert!(delegate.is_selectable(action_ix));
        assert_eq!(delegate.folded_section_at(action_ix), Some(action));
        assert!(delegate.is_header(action_ix + 1));
        assert!(!delegate.is_selectable(action_ix + 1));
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
            branch: None,
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
            Some("Awaiting their review")
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
            vec![SectionKind::Snoozed],
        );
        let copy = d.group_copy("Snoozed", ShareFormat::Markdown).unwrap();
        assert_eq!(copy.count, 1);
        assert!(copy
            .payload
            .plain
            .contains("(https://github.com/acme/widgets/pull/1)"));
        assert_eq!(
            d.group_copy("Awaiting their review", ShareFormat::Urls)
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
                ("My PR approved", Some(2)),
                ("Needs action", Some(1)),
                ("Awaiting their review", Some(2)),
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
            branch: None,
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
        // Only "Awaiting their review" — no empty Action/Draft headers.
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
                branch: None,
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
                branch: None,
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
        // Winter, summer, and both sides of Europe's clock changes. In a
        // DST zone, today's fixed offset cannot be right for every case.
        for timestamp in [
            "2026-01-12T12:00:00Z",
            "2026-09-12T12:00:00Z",
            "2026-03-29T00:30:00Z",
            "2026-03-29T01:30:00Z",
            "2026-10-25T00:30:00Z",
            "2026-10-25T01:30:00Z",
        ] {
            r.waiting_since = Some(timestamp.into());
            let instant = chrono::DateTime::parse_from_rfc3339(timestamp).unwrap();
            let since = instant.with_timezone(&Local).format("%Y-%m-%d %H:%M");
            let now = instant.with_timezone(&Utc) + chrono::Duration::days(3);
            let detail = detail_text_at(&r, Mode::Review, now);
            assert!(
                detail.contains(&format!("Waiting for a reviewer for 3d (since {since})")),
                "{timestamp}: {detail}"
            );
            let copied = row_copy_items(&r, Mode::Review)
                .into_iter()
                .find(|(label, _)| *label == "Copy all details")
                .unwrap()
                .1;
            assert!(copied.contains(&format!("(since {since})")), "{copied}");
            assert!(!detail.contains(timestamp), "no raw timestamp");
        }
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
            bot: false,
        }];
        r.stack = Some(prmarmot_core::board::StackInfo {
            number: 50,
            size: 3,
            position: Some(2),
            base_ref_name: "main".into(),
            branch: None,
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
            bot: false,
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

    #[test]
    fn a_range_takes_every_pr_between_two_rows_and_skips_the_headers() {
        let mut d = BoardTableDelegate::new(Mode::Authored, false);
        d.set_rows(vec![
            row(1, Category::Action),
            row(2, Category::Action),
            row(3, Category::Await),
            row(4, Category::Draft),
        ]);
        let first = d
            .display_index_of_url(&row(1, Category::Action).url)
            .unwrap();
        let last = d
            .display_index_of_url(&row(4, Category::Draft).url)
            .unwrap();
        assert!(last - first > 3, "headers lie between the sections");
        let ids = d.ids_between(last, first);
        assert_eq!(
            ids,
            (1..=4)
                .map(|n| format!("https://github.com/acme/widgets/pull/{n}"))
                .collect::<Vec<_>>(),
            "both ends in, display order, headers out"
        );
        d.set_multi(ids);
        assert_eq!(d.multi_len(), 4);
        assert_eq!(
            d.multi_rows().iter().map(|r| r.number).collect::<Vec<_>>(),
            [1, 2, 3, 4]
        );
        assert_eq!(d.visible_pr_ids().len(), 4);
    }

    #[test]
    fn a_toggle_grows_from_the_caret_and_one_row_left_is_the_caret_again() {
        let mut d = BoardTableDelegate::new(Mode::Authored, false);
        d.set_rows(vec![row(1, Category::Action), row(2, Category::Action)]);
        let (one, two) = (row(1, Category::Action).id, row(2, Category::Action).id);
        assert!(!d.multi_active());
        // ⌘-click #2 while the caret is on #1: both selected.
        assert!(d.toggle_multi(&two, Some(&one)));
        assert!(d.in_multi(&one) && d.in_multi(&two));
        // ⌘-click #2 again: it leaves, #1 alone is no longer a multi-selection.
        assert!(!d.toggle_multi(&two, Some(&one)));
        assert_eq!(d.multi_len(), 1);
        let caret = d.nearest_multi_index(d.display_index_of_url(&two).unwrap());
        assert_eq!(caret, d.display_index_of_url(&one));
        d.clear_multi();
        assert!(!d.multi_active());
    }

    #[test]
    fn a_refresh_drops_selected_prs_that_left_the_board() {
        let mut d = BoardTableDelegate::new(Mode::Authored, false);
        d.set_rows(vec![
            row(1, Category::Action),
            row(2, Category::Action),
            row(3, Category::Action),
        ]);
        d.set_multi(d.visible_pr_ids());
        assert_eq!(d.multi_len(), 3);
        d.set_rows(vec![row(3, Category::Action), row(1, Category::Action)]);
        assert_eq!(d.multi_len(), 2, "#2 merged; #1 and #3 stay selected");
        assert_eq!(
            d.multi_rows().iter().map(|r| r.number).collect::<Vec<_>>(),
            [3, 1],
            "in the new display order"
        );
        let copy = d.selection_copy(ShareFormat::Urls).unwrap();
        assert_eq!(copy.count, 2);
        assert_eq!(
            copy.payload.plain,
            "https://github.com/acme/widgets/pull/3\nhttps://github.com/acme/widgets/pull/1"
        );
        d.set_rows(vec![row(1, Category::Action)]);
        assert!(
            !d.multi_active(),
            "one row left is the caret row, not a selection"
        );
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

    fn glacier_fit(title_w: f32, note_w: f32, need: ContentNeed, all_repos: bool) -> (f32, f32) {
        let mut cols = vec![
            ColumnKind::Pr.column(PR_W),
            ColumnKind::Title.column(title_w),
            ColumnKind::Note.column(note_w),
        ];
        fit_to_content(&mut cols, need, all_repos);
        (f32::from(cols[1].width), f32::from(cols[2].width))
    }

    /// The split's two widths plus the scrollbar allowance, which Glacier's
    /// Note takes.
    const FIT_BUDGET: f32 = 400.0 + 600.0 + SCROLLBAR_MARGIN;

    #[test]
    fn glacier_gives_title_its_content_and_note_the_rest() {
        // Responsive split 400/600; the longest title needs 500 and the
        // widest Note 300: Title takes 512 (a quantum step up), Note the rest.
        let need = ContentNeed {
            title: 500.0,
            note: 300.0,
            note_primary: 120.0,
            title_max: 500.0,
            note_claim: 300.0,
        };
        assert_eq!(
            glacier_fit(400.0, 600.0, need, false),
            (512.0, FIT_BUDGET - 512.0)
        );
    }

    #[test]
    fn glacier_gives_title_its_width_first_and_elides_note_s_tail() {
        // Title needs 700 and the widest Note 500 of the 1024: Title wins
        // and Note's tail elides (Oliver, 2026-10-21: "Title first").
        let need = ContentNeed {
            title: 700.0,
            note: 500.0,
            note_primary: 160.0,
            title_max: 700.0,
            note_claim: 500.0,
        };
        assert_eq!(
            glacier_fit(400.0, 600.0, need, false),
            (704.0, FIT_BUDGET - 704.0)
        );
        // Short titles: Title shrinks to them (never under its minimum) and
        // Note gets the room.
        let need = ContentNeed {
            title: 100.0,
            note: 900.0,
            note_primary: 160.0,
            title_max: 100.0,
            note_claim: 900.0,
        };
        assert_eq!(
            glacier_fit(400.0, 600.0, need, false),
            (TITLE_MIN, FIT_BUDGET - TITLE_MIN)
        );
    }

    #[test]
    fn glacier_never_cuts_note_s_primary_phrase() {
        // Title wants everything; Note's widest primary phrase needs 300,
        // so Title stops where that stays whole (a quantum step up).
        let need = ContentNeed {
            title: 2000.0,
            note: 2000.0,
            note_primary: 300.0,
            title_max: 2000.0,
            note_claim: 2000.0,
        };
        assert_eq!(
            glacier_fit(400.0, 600.0, need, false),
            (FIT_BUDGET - 304.0, 304.0)
        );
        // A primary phrase wider than Title's minimum allows: Title keeps
        // its minimum and the phrase elides, as it must somewhere.
        let need = ContentNeed {
            title: 100.0,
            note: 2000.0,
            note_primary: 2000.0,
            title_max: 100.0,
            note_claim: 2000.0,
        };
        assert_eq!(
            glacier_fit(400.0, 600.0, need, false),
            (TITLE_MIN, FIT_BUDGET - TITLE_MIN)
        );
        assert!(!fit_to_content(
            &mut [ColumnKind::Pr.column(PR_W)],
            need,
            false
        ));
    }

    #[test]
    fn glacier_gives_note_s_floor_room_from_title_only_down_to_the_split() {
        // Title needs 900 and Note's floor phrase 800 of the 1024: Title
        // gives room back, but stops at its responsive 400, and the floor
        // phrase elides there (Oliver, 2026-10-21: "Title first, capped").
        let need = ContentNeed {
            title: 900.0,
            note: 900.0,
            note_primary: 800.0,
            title_max: 900.0,
            note_claim: 900.0,
        };
        assert_eq!(
            glacier_fit(400.0, 600.0, need, false),
            (400.0, FIT_BUDGET - 400.0)
        );
    }

    #[test]
    fn title_grows_past_its_claim_into_room_note_does_not_use() {
        // The claim is 300 and the widest title 500; the widest Note needs
        // 300 of the 1048, so Title takes its widest (a quantum step up)
        // and Note still has more than it uses.
        let need = ContentNeed {
            title: 300.0,
            note: 300.0,
            note_primary: 120.0,
            title_max: 500.0,
            note_claim: 300.0,
        };
        assert_eq!(
            glacier_fit(400.0, 600.0, need, false),
            (512.0, FIT_BUDGET - 512.0)
        );
        // A widest title wider than the room: Title takes what Note leaves.
        let need = ContentNeed {
            title: 300.0,
            note: 600.0,
            note_primary: 120.0,
            title_max: 900.0,
            note_claim: 600.0,
        };
        assert_eq!(
            glacier_fit(400.0, 600.0, need, false),
            (FIT_BUDGET - 608.0, 608.0)
        );
        // One outlier Note (900) does not hold Title: Note's claim (300)
        // is what counts, and the outlier elides.
        let need = ContentNeed {
            title: 300.0,
            note: 900.0,
            note_primary: 120.0,
            title_max: 500.0,
            note_claim: 300.0,
        };
        assert_eq!(
            glacier_fit(400.0, 600.0, need, false),
            (512.0, FIT_BUDGET - 512.0)
        );
    }

    #[test]
    fn title_claims_what_fits_most_rows_not_the_longest() {
        // Six rows: the widest. Seven or more: the 85th percentile, so one
        // outlier no longer widens the column.
        let mut six = [300.0, 310.0, 320.0, 330.0, 340.0, 900.0];
        assert_eq!(title_claim(&mut six), 900.0);
        let mut ten = [
            300.0, 310.0, 320.0, 330.0, 340.0, 350.0, 360.0, 370.0, 380.0, 900.0,
        ];
        assert_eq!(title_claim(&mut ten), 380.0);
        let mut twenty: Vec<f32> = (1..=20).map(|i| i as f32 * 10.0).collect();
        assert_eq!(title_claim(&mut twenty), 170.0);
        assert_eq!(title_claim(&mut []), 0.0);
    }

    #[test]
    fn note_floor_keeps_the_owner_s_first_action_phrase() {
        assert_eq!(
            note_floor_phrase("josefrichter's PR · merge conflict · 6 unresolved comments"),
            "josefrichter's PR · merge conflict"
        );
        assert_eq!(
            note_floor_phrase("oliver-kriska's PR · approved"),
            "oliver-kriska's PR · approved"
        );
        assert_eq!(note_floor_phrase("dependabot's PR"), "dependabot's PR");
        assert_eq!(note_floor_phrase("CI failing · 2 unresolved"), "CI failing");
        assert_eq!(note_floor_phrase("approved"), "approved");
    }

    #[test]
    fn columns_fit_within_viewport_budgets() {
        // Every width a table can have, in steps: the window's, or the window's
        // less the Details panel (a Medium table from 1120px up).
        let widths = (0..=87)
            .map(|i| 900.0 + 8.0 * i as f32)
            .chain([1919.0, 1920.0]);
        for w in widths {
            let class = TableWidthClass::from_width(w);
            for mode in [Mode::Authored, Mode::Review, Mode::AllOpen] {
                // All open always shows one repository.
                for all_repos in [false, mode != Mode::AllOpen] {
                    let cols = columns_for(mode, class, w, all_repos, false);
                    // Column widths + scrollbar margin must stay within the
                    // viewport; Note is last, so overflow would push it offscreen.
                    assert!(
                        total_width(&cols) + SCROLLBAR_MARGIN <= w + 1.0,
                        "mode {mode:?} all_repos={all_repos} at {w}px: total {} overflows",
                        total_width(&cols)
                    );
                }
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
    fn ticket_shows_on_a_wide_window_with_room_for_it_and_an_issue_rule() {
        for mode in [Mode::Authored, Mode::Review, Mode::AllOpen] {
            for all_repos in [false, mode != Mode::AllOpen] {
                for w in [1360.0_f32, 1440.0, 1600.0, 1780.0, 2400.0] {
                    let cols = columns_for(mode, TableWidthClass::Wide, w, all_repos, true);
                    assert!(
                        total_width(&cols) + SCROLLBAR_MARGIN <= w + 1.0,
                        "{mode:?} all_repos={all_repos} at {w}px: total {} overflows",
                        total_width(&cols)
                    );
                    if w >= 1600.0 {
                        assert_eq!(width_of(&cols, "ticket"), Some(TICKET_W), "{mode:?} {w}");
                    }
                }
                // No issue rule, or not a wide window: no Ticket column.
                let none = columns_for(mode, TableWidthClass::Wide, 1780.0, all_repos, false);
                assert_eq!(width_of(&none, "ticket"), None);
                let medium = columns_for(mode, TableWidthClass::Medium, 1300.0, all_repos, true);
                assert_eq!(width_of(&medium, "ticket"), None);
            }
        }
    }

    #[test]
    fn a_dragged_column_order_holds_in_every_view_and_keeps_hidden_columns_in_place() {
        use ColumnKind::*;
        let keys = |cols: &[Column]| {
            cols.iter()
                .filter_map(ColumnKind::of)
                .map(ColumnKind::key)
                .collect::<Vec<_>>()
        };
        // An order saved before Lines existed gets it after the column it
        // follows by default; a repeated column is dropped.
        assert_eq!(
            complete_order(vec![
                Title, Pr, Repo, Ticket, Ci, Review, Author, Unresolved, Labels, Note, Title
            ]),
            [Title, Pr, Repo, Ticket, Ci, Review, Author, Unresolved, Lines, Labels, Note]
        );
        // Moving Title before PR among the shown columns moves only those.
        assert_eq!(
            merged_order(&ColumnKind::ALL, &[Title, Pr, Ci, Note]),
            [Title, Repo, Ticket, Pr, Ci, Review, Author, Unresolved, Lines, Labels, Note]
        );

        let mut d = BoardTableDelegate::new(Mode::Authored, false);
        d.set_columns(columns_for(
            Mode::Authored,
            TableWidthClass::Medium,
            1200.0,
            false,
            false,
        ));
        let pr = d.kinds.iter().position(|k| *k == Some(Pr)).unwrap();
        let title = d.kinds.iter().position(|k| *k == Some(Title)).unwrap();
        let column = d.columns.remove(title);
        d.columns.insert(pr, column);
        let kind = d.kinds.remove(title);
        d.kinds.insert(pr, kind);
        let saved = d.remember_column_order();
        let at = |key: &str| saved.iter().position(|k| k == key).unwrap();
        assert!(at("title") < at("pr"), "{saved:?}");
        assert_eq!(saved.len(), ColumnKind::ALL.len());

        // Restored from the file, it orders another view's columns too.
        let mut restored = BoardTableDelegate::new(Mode::Review, false);
        restored.restore_column_order(Some(&saved));
        let review = restored.customize_columns(columns_for(
            Mode::Review,
            TableWidthClass::Medium,
            1200.0,
            false,
            false,
        ));
        assert_eq!(keys(&review)[..2], ["title", "pr"]);
        // Without a saved order each view keeps its own.
        let mut fresh = BoardTableDelegate::new(Mode::Review, false);
        fresh.restore_column_order(None);
        let own = fresh.customize_columns(columns_for(
            Mode::Review,
            TableWidthClass::Medium,
            1200.0,
            false,
            false,
        ));
        assert_eq!(keys(&own)[..2], ["pr", "title"]);
    }

    #[test]
    fn line_counts_stay_short() {
        assert_eq!(short_count(0), "0");
        assert_eq!(short_count(999), "999");
        assert_eq!(short_count(1_000), "1k");
        assert_eq!(short_count(1_249), "1.2k");
        assert_eq!(short_count(12_900), "12k");
        assert_eq!(short_count(999_999), "999k");
        assert_eq!(short_count(1_500_000), "1.5M");
    }

    #[test]
    fn every_view_shows_lines_on_a_wide_window() {
        for mode in [Mode::Authored, Mode::Review, Mode::AllOpen] {
            for (class, w) in [
                (TableWidthClass::Compact, 1000.0),
                (TableWidthClass::Medium, 1200.0),
                (TableWidthClass::Wide, 1600.0),
            ] {
                let cols = columns_for(mode, class, w, false, false);
                let lines = width_of(&cols, "lines");
                if class == TableWidthClass::Wide {
                    assert_eq!(lines, Some(LINES_W), "{mode:?} {class:?}");
                } else {
                    assert_eq!(lines, None, "{mode:?} {class:?}");
                }
            }
        }
    }

    #[test]
    fn wide_windows_give_labels_more_room() {
        for mode in [Mode::Authored, Mode::Review, Mode::AllOpen] {
            for all_repos in [false, true] {
                let medium = columns_for(mode, TableWidthClass::Medium, 1200.0, all_repos, false);
                let wide = columns_for(mode, TableWidthClass::Wide, 1360.0, all_repos, false);
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
    fn all_open_gives_review_the_notes_spare_room_on_wide_windows() {
        let wide = |w| columns_for(Mode::AllOpen, TableWidthClass::Wide, w, false, false);
        let review = |w| width_of(&wide(w), "review").unwrap();
        let title = |w| width_of(&wide(w), "title").unwrap();
        let note = |w| width_of(&wide(w), "note").unwrap();
        // At this width the extra Agents action and the Lines column leave
        // Review 288px while Title retains its quantized share of the
        // flexible space.
        assert_eq!(review(1780.0), 288.0);
        assert_eq!(title(1780.0), 400.0);
        // At the narrowest wide window Lines takes the Note's spare room.
        assert_eq!(review(1360.0), REVIEW_W);
        assert!(review(1500.0) > REVIEW_W);
        assert_eq!(review(2400.0), REVIEW_W_ALL_OPEN_MAX);
        for w in [1360.0_f32, 1500.0, 1780.0, 2400.0] {
            assert!(
                note(w) >= title(w),
                "at {w}px the Note narrows below the Title"
            );
            assert!(total_width(&wide(w)) + SCROLLBAR_MARGIN <= w + 1.0);
        }
        // The other views keep their Review width.
        assert_eq!(
            width_of(
                &columns_for(Mode::Authored, TableWidthClass::Wide, 1780.0, false, false),
                "review"
            ),
            Some(REVIEW_W)
        );
    }

    #[test]
    fn labels_present_except_in_compact() {
        for mode in [Mode::Authored, Mode::Review, Mode::AllOpen] {
            assert!(
                width_of(
                    &columns_for(mode, TableWidthClass::Compact, 1000.0, false, false),
                    "labels"
                )
                .is_none(),
                "{mode:?}: Labels should drop in Compact"
            );
            assert!(width_of(
                &columns_for(mode, TableWidthClass::Medium, 1200.0, false, false),
                "labels"
            )
            .is_some());
            assert!(width_of(
                &columns_for(mode, TableWidthClass::Wide, 1440.0, false, false),
                "labels"
            )
            .is_some());
        }
    }

    #[test]
    fn required_columns_never_disappear() {
        let required = |mode: Mode| -> &'static [&'static str] {
            match mode {
                Mode::Authored => &["pr", "title", "ci", "review", "unresolved", "note"],
                Mode::Review => &["pr", "title", "ci", "author", "unresolved", "note"],
                // Review folds into the Note at compact widths.
                Mode::AllOpen => &["pr", "title", "ci", "author", "unresolved", "note"],
            }
        };
        for &class in &ALL_CLASSES {
            for mode in [Mode::Authored, Mode::Review, Mode::AllOpen] {
                let cols = columns_for(mode, class, 900.0, false, false);
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
            for mode in [Mode::Authored, Mode::Review, Mode::AllOpen] {
                let cols = columns_for(mode, class, w, false, false);
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
        // All open is one repository's view; it never shows a Repo column.
        for mode in [Mode::Authored, Mode::Review] {
            let cols = columns_for(mode, TableWidthClass::Compact, 900.0, true, false);
            assert!(width_of(&cols, "repo").is_some());
            assert!(width_of(&cols, "note").unwrap() >= 224.0);
            assert!(total_width(&cols) + SCROLLBAR_MARGIN <= 900.0 + 1.0);
        }
    }

    #[test]
    fn override_storage_is_bounded_and_independent() {
        use std::collections::HashMap;
        let mut overrides: HashMap<(Mode, TableWidthClass, bool), Vec<Pixels>> = HashMap::new();
        for mode in [Mode::Authored, Mode::Review, Mode::AllOpen] {
            for &class in &ALL_CLASSES {
                for all_repos in [false, true] {
                    overrides.insert(
                        (mode, class, all_repos),
                        vec![px(mode as u8 as f32), px(1.0)],
                    );
                }
            }
        }
        // 3 modes × 3 classes × 2 scopes — the map can never hold more.
        assert_eq!(overrides.len(), 18);
        overrides.insert((Mode::Authored, TableWidthClass::Wide, true), vec![px(9.0)]);
        assert_eq!(overrides.len(), 18);
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
                        branch: None,
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
                    .filter(|row| matches_search(row, query, chips, rule()))
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
                delegate.set_attention(
                    HashMap::new(),
                    watched,
                    snoozed,
                    vec![SectionKind::Snoozed],
                );
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
