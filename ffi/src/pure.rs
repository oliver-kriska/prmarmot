//! The pure half of the boundary: everything that is a function of rows.
//!
//! These are the parts the iPad must not reimplement in Swift. Section order,
//! what `label:"help wanted"` means, how long a PR has been waiting, what goes
//! on the pasteboard — each one already has a golden test in `prmarmot-core`,
//! and a second implementation would drift from it within a release.

use std::collections::HashSet;

use prmarmot_core::cells as core_cells;
use prmarmot_core::detail as core_detail;
use prmarmot_core::layout as core_layout;
use prmarmot_core::pickup;
use prmarmot_core::search as core_search;
use prmarmot_core::share as core_share;
use prmarmot_core::size as core_size;
use prmarmot_core::status as core_status;

use crate::error::FfiError;
use crate::types::{
    instant, into_rows, BoardItem, ChangeSize, Ci, CiCell, FilterChip, FilterQualifier, Mode,
    NotePresentation, PullRequest, RateLimit, ReviewCell, SectionKind, ShareFormat, SharePayload,
    SizeBand, Sort, Tone,
};

/// Lay out a board: section headers in their fixed order, stack sub-groups,
/// the Approved split, and the trailing Snoozed group.
///
/// Rows are referenced by their index in `rows`, so the caller keeps whatever
/// it already had (a `SwiftUI` identity, a cell, a selection) instead of
/// receiving copies back. `snoozed` is the PR ids to move into the Snoozed
/// group, and `show_snoozed` shows those rows rather than only counting them.
#[uniffi::export]
pub fn layout(
    rows: Vec<PullRequest>,
    mode: Mode,
    all_repos: bool,
    snoozed: Vec<String>,
    show_snoozed: bool,
    sort: Sort,
) -> Vec<BoardItem> {
    let rows = into_rows(rows);
    let snoozed: HashSet<String> = snoozed.into_iter().collect();
    core_layout::layout(
        &rows,
        mode.into(),
        all_repos,
        &snoozed,
        show_snoozed,
        sort.into(),
    )
    .into_iter()
    .map(BoardItem::from)
    .collect()
}

/// [`layout`] with the sections in a person's order: `section_order` holds
/// the JSON section keys (`"available"`, `"await"` …), as the desktop's
/// `section_order` in config.toml does. Sections it leaves out follow in their
/// default order, and a key it cannot use is skipped (see
/// [`parse_section_order`] for what was skipped).
#[uniffi::export]
pub fn layout_ordered(
    rows: Vec<PullRequest>,
    mode: Mode,
    all_repos: bool,
    snoozed: Vec<String>,
    show_snoozed: bool,
    sort: Sort,
    section_order: Vec<String>,
) -> Vec<BoardItem> {
    let rows = into_rows(rows);
    let snoozed: HashSet<String> = snoozed.into_iter().collect();
    core_layout::layout_ordered(
        &rows,
        mode.into(),
        all_repos,
        &snoozed,
        show_snoozed,
        sort.into(),
        &core_layout::SectionOrder::from_keys(&section_order).0,
    )
    .into_iter()
    .map(BoardItem::from)
    .collect()
}

/// A section order as stored keys: every section, each once.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct SectionOrderChoice {
    /// Every orderable section's key, first to last: what to store.
    pub keys: Vec<String>,
    /// What of the input was skipped, one sentence each.
    pub ignored: Vec<String>,
}

/// Read a stored or typed section order: the sections it names first, then
/// the rest in their default order, and what it could not use.
#[uniffi::export]
pub fn parse_section_order(keys: Vec<String>) -> SectionOrderChoice {
    let (order, ignored) = core_layout::SectionOrder::from_keys(&keys);
    SectionOrderChoice {
        keys: order.keys().into_iter().map(str::to_owned).collect(),
        ignored,
    }
}

/// The default section order, as keys: Settings' "Reset to default".
#[uniffi::export]
pub fn default_section_order() -> Vec<String> {
    core_layout::SectionOrder::default()
        .keys()
        .into_iter()
        .map(str::to_owned)
        .collect()
}

/// `key` one place earlier or later in `keys`, a Settings list's up and down
/// buttons; at either end nothing moves. Returns the whole order, normalized.
#[uniffi::export]
pub fn move_section(keys: Vec<String>, key: String, earlier: bool) -> Vec<String> {
    let order = core_layout::SectionOrder::from_keys(&keys).0;
    let moved = match core_layout::ORDERABLE_SECTIONS
        .iter()
        .find(|kind| kind.key().eq_ignore_ascii_case(key.trim()))
    {
        Some(kind) => order.moved(*kind, earlier),
        None => order,
    };
    moved.keys().into_iter().map(str::to_owned).collect()
}

/// One line of a Settings section-order list.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct SectionOrderEntry {
    pub key: String,
    pub kind: SectionKind,
    /// "Awaiting review": the same in every view.
    pub name: String,
    /// Where it shows, e.g. "My PRs, Involving me, All open".
    pub views: String,
}

/// The rows of a Settings section-order list for `keys`, first to last, with
/// the words the desktop's list uses.
#[uniffi::export]
pub fn section_order_entries(keys: Vec<String>) -> Vec<SectionOrderEntry> {
    core_layout::SectionOrder::from_keys(&keys)
        .0
        .kinds()
        .iter()
        .map(|kind| SectionOrderEntry {
            key: kind.key().to_owned(),
            kind: (*kind).into(),
            name: core_layout::section_name(*kind).to_owned(),
            views: core_layout::section_views(*kind).to_owned(),
        })
        .collect()
}

/// The indices of the rows that match a search query, in the order given.
///
/// The grammar is the desktop search box's: bare words match the number,
/// repository, title, author, labels, linked issue and Note; `label:NAME`,
/// `author:LOGIN`, `repo:OWNER/NAME` and `is:stale` match a whole field;
/// values with spaces are quoted; every term must match, and case is ignored.
#[uniffi::export]
pub fn search(
    rows: Vec<PullRequest>,
    query: String,
    now_epoch: i64,
    stale_after_days: u64,
) -> Result<Vec<u32>, FfiError> {
    let rule = core_search::StaleRule {
        now: instant(now_epoch)?,
        after_days: stale_after_days,
    };
    let rows = into_rows(rows);
    Ok(rows
        .iter()
        .enumerate()
        .filter(|(_, row)| core_search::matches_filter(row, &query, rule))
        .map(|(index, _)| index as u32)
        .collect())
}

/// What a search field holds: chips, and the text still being typed.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FilterChips {
    pub chips: Vec<FilterChip>,
    /// What remains in the field after the chips are taken out.
    pub rest: String,
}

/// Split a query into chips and leftover text, the way the desktop field does
/// when you press Return (`finished`) or type (`finished == false`, which
/// leaves a half-typed last term alone).
#[uniffi::export]
pub fn take_filter_chips(query: String, finished: bool) -> FilterChips {
    let (chips, rest) = core_search::take_filter_chips(&query, finished);
    FilterChips {
        chips: chips.iter().map(FilterChip::from).collect(),
        rest,
    }
}

/// Add one term to a query, as tapping a label or an author does. Already
/// present, and the query comes back unchanged.
#[uniffi::export]
pub fn with_filter(query: String, qualifier: FilterQualifier, value: String) -> String {
    let chip = core_search::FilterChip::new(qualifier.into(), value);
    core_search::with_filter(&query, &chip)
}

/// What puts a PR in a section, in one sentence: the desktop's hover on a
/// section header, for the iPad's long-press. `None` for a stack sub-header.
#[uniffi::export]
pub fn section_explanation(mode: Mode, kind: SectionKind, all_repos: bool) -> Option<String> {
    core_layout::section_explanation(mode.into(), kind.into(), all_repos)
}

/// Seconds this PR has been waiting for a reviewer, or `None` when it is not
/// waiting on anyone.
#[uniffi::export]
pub fn waiting_secs(row: PullRequest, now_epoch: i64) -> Result<Option<u64>, FfiError> {
    let now = instant(now_epoch)?;
    Ok(pickup::waiting_secs(&row.into_row(), now))
}

/// A wait in the app's words: "<1h", "6h", "3d".
#[uniffi::export]
pub fn wait_label(secs: u64) -> String {
    pickup::wait_label(secs)
}

/// The size band of a change.
#[uniffi::export]
pub fn size_band(size: ChangeSize) -> SizeBand {
    core_size::ChangeSize::from(size).band().into()
}

/// "Small", "Medium", "Large": the band's name as the desktop shows it.
#[uniffi::export]
pub fn size_band_label(band: SizeBand) -> String {
    core_size::SizeBand::from(band).label().into()
}

/// "2 unresolved": open review threads as a short fact.
#[uniffi::export]
pub fn unresolved_label(count: u32) -> String {
    core_cells::unresolved_label(count as usize)
}

/// "42 changed lines in 3 files".
#[uniffi::export]
pub fn size_lines_and_files(size: ChangeSize) -> String {
    core_size::ChangeSize::from(size).lines_and_files()
}

/// Render one group for sharing — the same text the desktop's Copy group puts
/// on the clipboard. `title` is the group's heading, e.g. "Awaiting review".
#[uniffi::export]
pub fn share_group(
    title: String,
    rows: Vec<PullRequest>,
    mode: Mode,
    format: ShareFormat,
) -> SharePayload {
    let rows = into_rows(rows);
    core_share::share_group(&title, &rows, mode.into(), format.into()).into()
}

/// A Note with its leading status emoji removed.
///
/// Core's Note strings carry a glyph (`🔴`, `🟡`, `🟢`) because the shell
/// prototype did and the goldens pin it. Every graphical front end draws its
/// own dot instead, so it strips the glyph first — with this function, not
/// with a hand-written character check that will miss one.
#[uniffi::export]
pub fn strip_note_glyphs(note: String) -> String {
    prmarmot_core::board::strip_note_glyphs(&note)
}

/// The numbers the header sentence is built from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct HeaderCounts {
    pub loaded: u32,
    /// GitHub has more than this page.
    pub truncated: bool,
    pub mode: Mode,
    pub all_repos: bool,
    /// Rows of this view that need you (`row_needs_you`), snoozed ones
    /// excluded.
    pub need_you: u32,
    /// Across My PRs and the review queue: your PRs that need action plus
    /// reviews requested from you, snoozed ones excluded. This is the icon
    /// badge; All open adds nothing to it.
    pub badge: u32,
    /// Both views have loaded, so `badge` is the whole count.
    pub badge_complete: bool,
    /// Of every PR you watch or snoozed, in any view, how many the last
    /// refresh checked (up to 50 at a time) and how many there are. Only the
    /// explanation says these.
    pub tracked_loaded: u32,
    pub tracked_total: u32,
    /// `Board.total`: how many PRs GitHub's search found. All open's line
    /// reads "60 of 759 open" from it; the other views ignore it.
    #[uniffi(default = None)]
    pub total: Option<u64>,
    /// A `label:` or `author:` chip went to GitHub with All open's search, so
    /// `total` counts matches: "12 match".
    #[uniffi(default = false)]
    pub filtered: bool,
    /// Rows of this view you watch or snoozed: the line's "3 watched/snoozed".
    #[uniffi(default = 0)]
    pub followed: u32,
}

/// The header sentence and the explanation behind it.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct HeaderSummary {
    /// "56 loaded · partial results · 3 need you · 2 watched/snoozed".
    pub line: String,
    /// The paragraph behind the info button, with the badge named for iOS.
    pub explanation: String,
}

/// The header sentence, in the desktop's words with the iPad's badge name.
#[uniffi::export]
pub fn header_summary(counts: HeaderCounts) -> HeaderSummary {
    let (line, explanation) = core_status::header_counts(
        &core_status::HeaderCounts {
            loaded: counts.loaded as usize,
            truncated: counts.truncated,
            mode: counts.mode.into(),
            all_repos: counts.all_repos,
            need_you: counts.need_you as usize,
            badge: counts.badge as usize,
            badge_complete: counts.badge_complete,
            followed: counts.followed as usize,
            tracked_loaded: counts.tracked_loaded as usize,
            tracked_total: counts.tracked_total as usize,
            total: counts.total,
            filtered: counts.filtered,
        },
        core_status::BadgeName::AppIcon,
    );
    HeaderSummary { line, explanation }
}

/// Whether a row of this category can count toward "need you", from the
/// category alone. Needs action in Involving me and All open also holds other
/// people's PRs, so count rows with `row_needs_you`.
#[uniffi::export]
pub fn needs_you_here(mode: Mode, category: crate::types::Category) -> bool {
    core_status::needs_you_here(mode.into(), category.into())
}

/// Whether this row counts toward "need you": the icon badge's rule in every
/// view. Requested from you plus your own PRs under Needs action; never a
/// teammate's, and never Available to review.
#[uniffi::export]
pub fn row_needs_you(mode: Mode, row: PullRequest) -> bool {
    core_status::row_needs_you(mode.into(), &row.into_row())
}

/// What Load more added, said once it lands: its rows join their sections
/// rather than the bottom of the list.
#[uniffi::export]
pub fn loaded_more_text(added: u32) -> String {
    core_status::loaded_more_text(added as usize)
}

/// What the Changed toggle says it will do.
#[uniffi::export]
pub fn changed_toggle_text(on: bool, count: u32) -> String {
    core_status::changed_toggle_tooltip(on, count as usize)
}

/// What the Snoozed toggle says it will do.
#[uniffi::export]
pub fn snoozed_toggle_text(on: bool, count: u32) -> String {
    core_status::snoozed_toggle_tooltip(on, count as usize)
}

/// The blue marker's explanation: what changed, and how to clear it.
#[uniffi::export]
pub fn changed_marker_text(changes: Vec<String>) -> String {
    core_status::changed_marker_tooltip(&changes)
}

/// "just now", "3m ago", "2h 15m ago".
#[uniffi::export]
pub fn relative_time(secs_ago: i64) -> String {
    core_status::relative(secs_ago)
}

/// "45s", "10m", "1h 5m" — a wait ahead.
#[uniffi::export]
pub fn human_duration(secs: u64) -> String {
    core_status::human_duration(secs)
}

/// The queue's status line, keeping "synced Xm ago" visible while refreshing.
#[uniffi::export]
pub fn queue_sync_text(
    mode: Mode,
    all_repos: bool,
    syncing: bool,
    synced_secs_ago: Option<i64>,
) -> String {
    core_status::queue_sync_text(mode.into(), all_repos, syncing, synced_secs_ago)
}

/// What to show before a queue's first rows ever arrive.
#[uniffi::export]
pub fn queue_loading_text(mode: Mode, all_repos: bool) -> String {
    core_status::queue_loading_text(mode.into(), all_repos).to_owned()
}

/// What to show when a queue has loaded and holds nothing.
#[uniffi::export]
pub fn queue_empty_text(mode: Mode, all_repos: bool) -> String {
    core_status::queue_empty_text(mode.into(), all_repos).to_owned()
}

/// Why All open is unavailable with all repositories selected — for the
/// disabled tab and the error alike.
#[uniffi::export]
pub fn all_open_needs_repository() -> String {
    core_status::all_open_needs_repository().to_owned()
}

/// All open's empty body when GitHub answered the whole filter (or every
/// open PR is loaded) and nothing matched. `filter` is the search as shown.
#[uniffi::export]
pub fn all_open_no_match_text(filter: String) -> String {
    core_status::all_open_no_match_text(&filter)
}

/// The terms of a search that All open checks only against the loaded rows —
/// free words (in quotes) and the chips GitHub is not sent — as the reader
/// would recognise them.
#[uniffi::export]
pub fn local_only_terms(query: String, chips: Vec<FilterChip>) -> Vec<String> {
    let chips: Vec<core_search::FilterChip> = chips
        .iter()
        .map(|chip| core_search::FilterChip::new(chip.qualifier.into(), &chip.value))
        .collect();
    let remote = core_search::RemoteFilter::from_chips(&chips);
    core_search::local_only_terms(&query, &chips, &remote)
}

/// The line under All open's filter when part of it only looked at the
/// loaded PRs and GitHub has more. `None` when nothing needs saying.
#[uniffi::export]
pub fn all_open_local_filter_notice(
    local_terms: Vec<String>,
    loaded: u32,
    total: Option<u64>,
    can_load_more: bool,
) -> Option<String> {
    core_status::all_open_local_filter_notice(&local_terms, loaded as usize, total, can_load_more)
}

/// One entry of the Details panel's Copy menu.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct CopyItem {
    /// What the menu says, e.g. "Copy PR reference".
    pub label: String,
    /// What goes on the pasteboard.
    pub text: String,
}

/// Every line of the Details panel, in the order it is shown.
///
/// The desktop draws the same list from the same function: "the same facts in
/// the same words" is a property of this call, not of two hand-kept copies.
/// `tz_offset_secs` is the reader's offset from UTC — only the "(since …)"
/// timestamp uses it.
#[uniffi::export]
pub fn detail_lines(
    row: PullRequest,
    mode: Mode,
    now_epoch: i64,
    tz_offset_secs: i32,
) -> Result<Vec<String>, FfiError> {
    let now = instant(now_epoch)?;
    Ok(core_detail::detail_lines(
        &row.into_row(),
        mode.into(),
        now,
        tz_offset_secs,
    ))
}

/// What a line of the Details panel is (`detail_items`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum DetailKind {
    Note,
    Facts,
    RequestedReviewers,
    Reviews,
    YourReview,
    Waiting,
    Size,
    Labels,
    Issue,
    Stack,
    Snapshot,
}

impl From<core_detail::DetailKind> for DetailKind {
    fn from(kind: core_detail::DetailKind) -> Self {
        use core_detail::DetailKind as Core;
        match kind {
            Core::Note => Self::Note,
            Core::Facts => Self::Facts,
            Core::RequestedReviewers => Self::RequestedReviewers,
            Core::Reviews => Self::Reviews,
            Core::YourReview => Self::YourReview,
            Core::Waiting => Self::Waiting,
            Core::Size => Self::Size,
            Core::Labels => Self::Labels,
            Core::Issue => Self::Issue,
            Core::Stack => Self::Stack,
            Core::Snapshot => Self::Snapshot,
        }
    }
}

/// One line of the Details panel and what it is.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct DetailLine {
    pub kind: DetailKind,
    pub text: String,
}

/// [`detail_lines`], each with what it is, so the labels can be chips and
/// the closing line muted without the front end matching on the words.
#[uniffi::export]
pub fn detail_items(
    row: PullRequest,
    mode: Mode,
    now_epoch: i64,
    tz_offset_secs: i32,
) -> Result<Vec<DetailLine>, FfiError> {
    let now = instant(now_epoch)?;
    Ok(
        core_detail::detail_items(&row.into_row(), mode.into(), now, tz_offset_secs)
            .into_iter()
            .map(|(kind, text)| DetailLine {
                kind: kind.into(),
                text,
            })
            .collect(),
    )
}

/// "Attention: changed · watched", the Details panel's line under core's.
#[uniffi::export]
pub fn attention_line(changed: bool, watched: bool) -> String {
    core_detail::attention_line(changed, watched)
}

/// The Details panel's Copy menu, in the desktop's order and wording.
#[uniffi::export]
pub fn copy_items(
    row: PullRequest,
    mode: Mode,
    now_epoch: i64,
    tz_offset_secs: i32,
) -> Result<Vec<CopyItem>, FfiError> {
    let now = instant(now_epoch)?;
    Ok(
        core_detail::copy_items(&row.into_row(), mode.into(), now, tz_offset_secs)
            .into_iter()
            .map(|(label, text)| CopyItem {
                label: label.to_owned(),
                text,
            })
            .collect(),
    )
}

/// How long to wait after GitHub says the budget is spent, in seconds.
///
/// The clamp is core's and the reason is the PRFlow post-mortem: a far-future
/// or garbage reset time must not freeze the refresh loop, and a reset ten
/// seconds away must not turn into a poll every ten seconds. Always between a
/// minute and fifteen.
#[uniffi::export]
pub fn backoff_secs(reset_epoch: Option<u64>, now_epoch: u64) -> u64 {
    prmarmot_core::github::rate_limit::backoff_secs(reset_epoch, now_epoch)
}

/// How long to wait after a request failed with `FfiError::RateLimited`, in
/// seconds: `retry_after_secs` when GitHub sent it, otherwise until
/// `reset_epoch`, between a minute and fifteen. The desktop waits by the same
/// function. A clock before 1970 reads as 1970.
#[uniffi::export]
pub fn rate_limited_wait_secs(
    reset_epoch: Option<u64>,
    retry_after_secs: Option<u64>,
    now_epoch: i64,
) -> u64 {
    prmarmot_core::github::rate_limit::rate_limited_wait_secs(
        reset_epoch,
        retry_after_secs,
        u64::try_from(now_epoch).unwrap_or(0),
    )
}

/// Whether to stop fetching to leave GitHub's budget for the person's own
/// tools, and until when: the epoch second to wait until, or `None` to fetch.
///
/// The desktop calls the same core function, so both products pause on the
/// same numbers. It pauses when fewer than [`rate_limit_reserve`] points remain
/// and the budget has not refilled yet, for between one and fifteen minutes.
/// A reset of 0 or less is unknown (see `RateLimit::reset_epoch`), and an
/// unknown or past reset never pauses: the next fetch brings a fresh budget.
/// Pass the `RateLimit` of the last board and the caller's clock.
#[uniffi::export]
pub fn reserve_pause_until(rate: RateLimit, now_epoch: i64) -> Option<i64> {
    let reset = u64::try_from(rate.reset_epoch)
        .ok()
        .filter(|&reset| reset > 0);
    let now = u64::try_from(now_epoch).unwrap_or(0);
    prmarmot_core::github::rate_limit::reserve_pause_until(rate.remaining, reset, now)
        .map(|until| i64::try_from(until).unwrap_or(i64::MAX))
}

/// How many points of GitHub's hourly budget PR Marmot leaves for the
/// person's own `gh` and git: fetching pauses below this.
#[uniffi::export]
pub fn rate_limit_reserve() -> u32 {
    prmarmot_core::github::rate_limit::RATE_LIMIT_RESERVE
}

/// The label a section header shows for a category, e.g. "Needs action".
#[uniffi::export]
pub fn group_label(mode: Mode, category: crate::types::Category, all_repos: bool) -> String {
    core_layout::group_label(mode.into(), category.into(), all_repos).to_owned()
}

/// The Note cell for one row: which phrase is emphasised, which facts trail it
/// muted, and how alarming the whole thing is.
///
/// The iPad must not decide this for itself. Whether a merge conflict outranks
/// three unresolved comments, and whether that is red or amber, is the same
/// judgement on both products or they are two products.
#[uniffi::export]
pub fn note_presentation(row: PullRequest) -> NotePresentation {
    core_cells::note_presentation(&row.into_row()).into()
}

/// The Review cell for one row: who reviewed and with what mark, or who was
/// asked, or that nobody was.
#[uniffi::export]
pub fn review_cell(row: PullRequest) -> ReviewCell {
    core_cells::review_cell(&row.into_row()).into()
}

/// The CI cell: the word and its tone. `None` is an em dash, never an empty
/// cell, because "no checks" and "not loaded" must not look the same.
#[uniffi::export]
pub fn ci_cell(ci: Ci) -> CiCell {
    let (text, tone) = core_cells::ci_cell(ci.into());
    CiCell {
        text: text.to_owned(),
        tone: tone.into(),
    }
}

/// All open only: the labels more than half of the rows on screen carry (at
/// least ten rows), which a row's chips draw last. Other views pass none.
#[uniffi::export]
pub fn common_labels(rows: Vec<PullRequest>) -> Vec<String> {
    core_cells::common_labels(&into_rows(rows))
}

/// The order a row's label chips are drawn in: `bug` first, then the rest as
/// GitHub lists them, then the `common` ones, so the chips that fit say the
/// most.
#[uniffi::export]
pub fn label_order(labels: Vec<String>, common: Vec<String>) -> Vec<String> {
    core_cells::label_order(&labels, &common)
}

/// How alarming a section is, so that a front end can decorate its heading
/// without deciding for itself that "needs action" outranks "awaiting review".
#[uniffi::export]
pub fn section_tone(kind: SectionKind) -> Tone {
    core_cells::section_tone(&kind.into()).into()
}

/// The branch prefix drawn before a stacked row's title: `└─ 3/3` for the last
/// layer shown in its group, `├─ 2/3` for one with more below it.
#[uniffi::export]
pub fn stack_branch(position: Option<u64>, size: u64, last_in_group: bool) -> String {
    core_cells::stack_branch(position, size, last_in_group)
}

/// Why a stack may look incomplete: the layers that do not match the current
/// view are in other sections, not missing.
#[uniffi::export]
pub fn stack_layer_hover(
    number: u64,
    position: Option<u64>,
    size: u64,
    base_ref: String,
) -> String {
    core_cells::stack_layer_hover(number, position, size, &base_ref)
}
