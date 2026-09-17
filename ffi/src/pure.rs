//! The pure half of the boundary: everything that is a function of rows.
//!
//! These are the parts the iPad must not reimplement in Swift. Section order,
//! what `label:"help wanted"` means, how long a PR has been waiting, what goes
//! on the pasteboard — each one already has a golden test in `prmarmot-core`,
//! and a second implementation would drift from it within a release.

use std::collections::HashSet;

use prmarmot_core::detail as core_detail;
use prmarmot_core::layout as core_layout;
use prmarmot_core::pickup;
use prmarmot_core::search as core_search;
use prmarmot_core::share as core_share;
use prmarmot_core::size as core_size;

use crate::error::FfiError;
use crate::types::{
    instant, into_rows, BoardItem, ChangeSize, FilterChip, FilterQualifier, Mode, PullRequest,
    ShareFormat, SharePayload, SizeBand, Sort,
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

/// The label a section header shows for a category, e.g. "Needs attention".
#[uniffi::export]
pub fn group_label(mode: Mode, category: crate::types::Category, all_repos: bool) -> String {
    core_layout::group_label(mode.into(), category.into(), all_repos).to_owned()
}
