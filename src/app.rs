//! Root view: header (repo, counts, sync + rate-limit status) over the board
//! table, the auto-refresh loop, and the keyboard/mouse actions.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use chrono::{DateTime, Duration as ChronoDuration, Local, Utc};
use gpui::prelude::FluentBuilder;
use gpui::{
    div, px, AnyElement, App, AppContext, ClipboardItem, Context, Entity, FocusHandle, Focusable,
    FontWeight, InteractiveElement, IntoElement, KeyBinding, KeyDownEvent, ParentElement, Pixels,
    Render, SharedString, StatefulInteractiveElement, Styled, Window,
};
use gpui_base::SelectableText;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::{DropdownMenu, PopupMenuItem};
use gpui_component::select::{SearchableVec, Select, SelectEvent, SelectState};
use gpui_component::tab::{Tab, TabBar};
use gpui_component::table::{Column, DataTable, TableEvent, TableState};
use gpui_component::tooltip::Tooltip;
use gpui_component::{
    h_flex, v_flex, ActiveTheme, Disableable, IndexPath, Sizable, TitleBar, WindowExt,
};
use prmarmot_core::board::{BoardRow, BoardScope, Mode};
use prmarmot_core::layout::{SectionKind, SectionOrder, Sort, COLLAPSIBLE_SECTIONS};
use prmarmot_core::search::{
    local_only_terms, query_board, BoardQuery, QuickCounts, RemoteFilter, Search,
};
use prmarmot_core::status::{
    all_open_local_filter_notice, all_open_needs_repository, all_open_no_match_text,
    no_loaded_match_text,
};
use prmarmot_local::config::{collapse_view, DetailsPosition, DEFAULT_COLLAPSED};

use crate::design::type_size;
use crate::state::{AppState, SetupStatus};
use crate::table::{
    changed_marker_tooltip, columns_for, label_chip, take_filter_chips, with_filter,
    BoardTableDelegate, ClickIntent, FilterChip, Qualifier, RowAttention, StaleRule,
    TableWidthClass,
};
use crate::theme::ThemePref;
use crate::updates::{AutomaticCheck, CheckResult, InstallChannel, StableVersion};

mod chrome;
mod details;
mod pump;
mod setup;
mod updates;

// `FocusSearch` (⌘F / Ctrl-F, Edit → Find) is bound in `main`.
gpui::actions!(prmarmot, [CloseDetails, FocusSearch]);

const ALL_REPOS_LABEL: &str = "All repositories";

fn search_placeholder(mode: Mode) -> &'static str {
    match mode {
        Mode::AllOpen => "Filter open PRs…",
        Mode::Authored | Mode::Review => "Filter loaded PRs…",
    }
}
/// Chips (`label:`, `author:`, `repo:`, `is:`) the search box holds at most.
const MAX_FILTER_CHIPS: usize = 8;

fn scope_label(scope: &BoardScope) -> String {
    match scope {
        BoardScope::AllRepositories => ALL_REPOS_LABEL.to_owned(),
        BoardScope::Repository(repo) => repo.clone(),
    }
}

/// The footer's repository count: the repositories in the picker, not
/// counting its "All repositories" entry.
/// The Pin button's tooltip when every pin is taken.
fn pin_limit_text() -> String {
    format!(
        "Unpin a repository first ({} pins maximum)",
        crate::config::MAX_PINNED_REPOS
    )
}

fn repo_count_status(count: usize, truncated: bool) -> String {
    format!(
        "{count} {}{}",
        if count == 1 {
            "repository"
        } else {
            "repositories"
        },
        if truncated {
            " · discovery limit reached"
        } else {
            ""
        }
    )
}

fn scope_from_label(label: &str) -> BoardScope {
    if label == ALL_REPOS_LABEL {
        BoardScope::AllRepositories
    } else {
        BoardScope::Repository(label.to_owned())
    }
}

/// Startup decisions resolved in `main` (CLI + env + config file).
pub struct Launch {
    pub theme: ThemePref,
    pub refresh: Duration,
    /// Repo-picker entries; the active repo is always among them.
    pub repos: Vec<String>,
    pub pinned_repos: Vec<String>,
    pub automatic_update_checks: bool,
    pub update_paths: crate::config::UpdatePaths,
    pub update_failure: Option<String>,
    /// What the settings ignored: the whole file, or single values.
    pub config_warnings: crate::config::ConfigWarnings,
    /// `section_order` from the file.
    pub section_order: SectionOrder,
    /// `[collapsed_sections]` from the file, per view.
    pub collapsed: CollapsedSections,
    /// `details_position` from the file.
    pub details_position: DetailsPosition,
}

/// The right-hand Details panel's width (the iPad's inspector is 360–400 pt).
const DETAILS_PANEL_WIDTH: f32 = 360.;
/// The label column of its rows: "Requested reviewers" on one line.
const DETAILS_LABEL_WIDTH: f32 = 124.;

/// Which sections each view shows collapsed, keyed by the
/// `[collapsed_sections]` view names; bounded by the four views.
pub type CollapsedSections = HashMap<&'static str, Vec<SectionKind>>;

/// Every view's collapsed sections as the file says.
pub fn collapsed_from(file: &prmarmot_local::config::FileConfig) -> CollapsedSections {
    prmarmot_local::config::COLLAPSE_VIEWS
        .into_iter()
        .map(|view| (view, prmarmot_local::config::collapsed_sections(file, view)))
        .collect()
}

/// Persist the current window size so the next launch opens the same way.
/// Only the plain-windowed size — maximized/fullscreen store their restore
/// size, which is what we'd want back anyway.
pub(crate) fn save_window_size(window: &Window) {
    let (gpui::WindowBounds::Windowed(bounds)
    | gpui::WindowBounds::Maximized(bounds)
    | gpui::WindowBounds::Fullscreen(bounds)) = window.window_bounds();
    crate::config::persist_window(bounds.size.width.into(), bounds.size.height.into());
}

pub struct RootView {
    state: Entity<AppState>,
    table: Entity<TableState<BoardTableDelegate>>,
    repo_select: Entity<SelectState<SearchableVec<String>>>,
    configured_repos: Vec<String>,
    pinned_repos: Vec<String>,
    search_open: bool,
    discovering_repos: bool,
    /// Discovery has run once (or is running); later runs are the Repos
    /// button's.
    repos_discovered: bool,
    repo_status: String,
    search: Entity<InputState>,
    /// The view the search box's placeholder was last written for.
    placeholder_mode: Mode,
    /// The words typed in the search box.
    filter_text: String,
    /// The search box's chips, ANDed with the typed words.
    filter_chips: Vec<FilterChip>,
    visible_count: usize,
    details_open: bool,
    focus_handle: FocusHandle,
    seen_generation: u64,
    theme_pref: ThemePref,
    refresh: Duration,
    refresh_task: Option<gpui::Task<()>>,
    feedback: Option<SharedString>,
    feedback_task: Option<gpui::Task<()>>,
    /// Selected PR per queue, keyed by stable identity (URL), restored on
    /// switch-back so a queue keeps its place (critique #1). Stored by URL —
    /// NOT display index — so a background refresh that inserts/removes/reorders
    /// rows can never silently reselect a different PR or a header. Bounded by
    /// the number of modes.
    selections: HashMap<Mode, String>,
    /// Set on a mode switch and consumed by the generation observer once the
    /// switched rows land, so selection restore happens after the rows exist.
    pending_restore: Option<Mode>,
    /// Last real (non-header) selected row, so header bounces know which way
    /// the caret was travelling. Reset on any queue/repo switch.
    last_selected: usize,
    /// Where a ⇧-click or ⇧↑/↓ range starts: the row of the last plain or
    /// ⌘ select. Reset with the selection.
    selection_anchor: Option<usize>,
    /// The current responsive width bucket, updated on window resize.
    width_class: TableWidthClass,
    /// Last-seen viewport width (px), the basis for the elastic Title/Note split.
    viewport_width: f32,
    /// Manually-resized column widths, remembered per (queue, width class) so a
    /// background refresh or a same-class window resize can't reset a layout the
    /// user dragged. Bounded by construction: 3 views × 3 classes × 2 scopes
    /// = 18 entries.
    col_overrides: HashMap<(Mode, TableWidthClass, bool), Vec<Pixels>>,
    changed_only: bool,
    /// The Needs you quick filter: only the rows the header counts as "need
    /// you". Like Changed, it holds across views.
    needs_you_only: bool,
    /// Which sections each view shows collapsed (`[collapsed_sections]`).
    collapsed: CollapsedSections,
    /// Where Details opens (`details_position`, Settings).
    details_position: DetailsPosition,
    /// Review queue: list the smallest changes first instead of the longest
    /// wait.
    smallest_first: bool,
    /// The order sections come in, in every view (`section_order`, Settings).
    section_order: SectionOrder,
    /// The quick filters' and the header's numbers over the loaded rows,
    /// counted in `sync_table`, not per frame.
    counts: QuickCounts,
    suppress_ack_for: Option<String>,
    /// The update check and the update it may offer (`app/updates.rs`).
    updates: updates::Updates,
    config_warnings: crate::config::ConfigWarnings,
    notification_help_shown: bool,
    /// Delivered notifications whose click is awaited, oldest first, at most
    /// [`crate::platform::MAX_NOTIFICATION_WAITERS`]: dropping a wait cancels
    /// it and leaves the notification in Notification Center.
    #[cfg(target_os = "macos")]
    notification_clicks: std::collections::VecDeque<gpui::Task<()>>,
    /// The sign-in screen shown when neither a stored token nor `gh` works.
    onboarding: Entity<crate::onboarding::OnboardingView>,
}

impl RootView {
    pub fn new(
        state: Entity<AppState>,
        launch: Launch,
        auth: prmarmot_local::config::AuthSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // Override the table's Escape-to-clear-selection only while details is open.
        cx.bind_keys([KeyBinding::new(
            "escape",
            CloseDetails,
            Some("PrmarmotDetails > DataTable"),
        )]);
        let mode = state.read(cx).mode;
        let all_repos = state.read(cx).scope.is_all();
        // Size the columns to the window the moment we open, so the first frame
        // is already responsive (no default-width flash).
        let initial_width: f32 = window.viewport_size().width.into();
        let initial_class = TableWidthClass::from_width(initial_width);
        let table = Self::build_table(mode, all_repos, initial_class, initial_width, window, cx);
        let repo_select = Self::build_repo_select(&state, launch.repos.clone(), window, cx);
        let search = Self::build_search(mode, window, cx);

        // Theme: apply the configured preference, and while in System mode
        // follow macOS appearance changes live.
        let theme_pref = launch.theme;
        theme_pref.apply(window, cx);
        let this_handle = cx.entity().downgrade();
        window
            .observe_window_appearance(move |window, cx| {
                if let Some(view) = this_handle.upgrade() {
                    if view.read(cx).theme_pref == ThemePref::System {
                        ThemePref::System.apply(window, cx);
                    }
                }
            })
            .detach();

        Self::observe_state(&state, window, cx);

        cx.subscribe(&table, |this, _table, event: &TableEvent, cx| match event {
            TableEvent::DoubleClickedRow(row_ix) => this.open_row(*row_ix, cx),
            TableEvent::SelectRow(row_ix) => this.on_select_row(*row_ix, cx),
            TableEvent::ClearSelection => {
                this.clear_multi_selection(cx);
            }
            TableEvent::ColumnWidthsChanged(widths) => {
                this.on_column_widths_changed(widths.clone(), cx)
            }
            _ => {}
        })
        .detach();

        // Rebuild columns when the window is resized (class change, or a
        // material change in the elastic Title/Note widths). Event-driven — no
        // timer, nothing animates.
        cx.observe_window_bounds(window, |this, window, cx| this.relayout(window, cx))
            .detach();

        // Arrow keys belong to the table's own key context.
        table.focus_handle(cx).focus(window, cx);

        #[cfg(feature = "perf")]
        crate::perf::start(window, cx);

        // The board is the app: closing its window (red traffic light, ⌘W)
        // quits, the same as `q`. Left running with no window, the refresh
        // ticker would die with the window and a Dock click would reopen
        // nothing (the app registers no reopen handler), leaving a live
        // process that does nothing until Quit from the Dock menu.
        window.on_window_should_close(cx, |window, cx| {
            save_window_size(window);
            cx.quit();
            true
        });

        Self::start_ticker(window, cx);
        Self::start_event_pump(state.read(cx).platform_event_signal(), window, cx);

        let onboarding = cx.new(|cx| crate::onboarding::OnboardingView::new(auth, window, cx));
        // A completed sign-in re-runs the setup check, which picks up the new
        // token and swaps the transport. A host typed in the Enterprise field
        // becomes the one the app connects to, now and after a restart.
        cx.subscribe(
            &onboarding,
            |this: &mut Self, _, signed_in: &crate::onboarding::SignedIn, cx| {
                this.state.update(cx, |state, cx| {
                    if state.connector().use_host(&signed_in.host) {
                        crate::config::persist_auth_host(&signed_in.host);
                    }
                    state.validate_setup(cx)
                });
            },
        )
        .detach();

        let mut this = Self {
            state,
            table,
            repo_select,
            configured_repos: launch.repos,
            pinned_repos: launch.pinned_repos,
            search_open: false,
            placeholder_mode: mode,
            discovering_repos: false,
            repos_discovered: false,
            repo_status: String::new(),
            search,
            filter_text: String::new(),
            filter_chips: Vec::new(),
            visible_count: 0,
            details_open: false,
            focus_handle: cx.focus_handle(),
            seen_generation: 0,
            theme_pref,
            refresh: launch.refresh,
            refresh_task: None,
            feedback: None,
            feedback_task: None,
            selections: HashMap::new(),
            pending_restore: None,
            last_selected: 0,
            selection_anchor: None,
            width_class: initial_class,
            viewport_width: initial_width,
            col_overrides: HashMap::new(),
            changed_only: false,
            needs_you_only: false,
            collapsed: launch.collapsed,
            details_position: launch.details_position,
            smallest_first: false,
            section_order: launch.section_order,
            counts: QuickCounts::default(),
            suppress_ack_for: None,
            updates: updates::Updates {
                automatic: launch.automatic_update_checks,
                paths: launch.update_paths,
                available: None,
                error: launch.update_failure,
                check_pending: false,
                starting: false,
                check_task: None,
            },
            config_warnings: launch.config_warnings,
            notification_help_shown: false,
            #[cfg(target_os = "macos")]
            notification_clicks: std::collections::VecDeque::new(),
            onboarding,
        };
        if this.state.read(cx).attention_preferences.notifications {
            this.state.read(cx).check_notification_permission();
        }
        this.state.update(cx, |state, cx| state.validate_setup(cx));
        this.start_refresh_loop(cx);
        this.check_for_updates(cx);
        this.start_update_check_loop(cx);
        this
    }

    /// Open the search bar with its text selected, so typing replaces it.
    fn open_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search_open = true;
        self.search.update(cx, |input, cx| {
            input.focus(window, cx);
            input.select_all(window, cx);
        });
        cx.notify();
    }

    /// Clear the words and chips and close the search box.
    fn close_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // `set_value` emits no change event; update the filter here.
        self.search
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.filter_text.clear();
        self.filter_chips.clear();
        self.search_open = false;
        self.sync_table(cx);
        self.table.focus_handle(cx).focus(window, cx);
    }

    /// Whether the board is filtered by the search box.
    fn filtering(&self) -> bool {
        !self.filter_text.trim().is_empty() || !self.filter_chips.is_empty()
    }

    /// The search as one line, chips first, for messages.
    fn filter_summary(&self) -> String {
        let chips = self
            .filter_chips
            .iter()
            .fold(String::new(), |query, chip| with_filter(&query, chip));
        format!("{chips} {}", self.filter_text.trim())
            .trim()
            .to_owned()
    }

    fn add_filter_chip(&mut self, chip: FilterChip, cx: &mut Context<Self>) {
        if self.filter_chips.iter().any(|held| held.same_as(&chip)) {
            return;
        }
        if self.filter_chips.len() >= MAX_FILTER_CHIPS {
            self.show_feedback(
                format!("Search holds at most {MAX_FILTER_CHIPS} filters"),
                cx,
            );
            return;
        }
        self.filter_chips.push(chip);
    }

    fn remove_filter_chip(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index < self.filter_chips.len() {
            self.filter_chips.remove(index);
            // Unless you're typing, the keyboard goes back to the board, and
            // with nothing left the empty box goes away.
            if !self.search.focus_handle(cx).is_focused(window) {
                if !self.filtering() {
                    self.search_open = false;
                }
                self.table.focus_handle(cx).focus(window, cx);
            }
            self.sync_table(cx);
        }
    }

    /// The sections the view on screen shows collapsed.
    fn collapsed_here(&self, state: &AppState) -> &[SectionKind] {
        let view = collapse_view(state.mode, state.scope.is_all());
        self.collapsed
            .get(view)
            .map(Vec::as_slice)
            .unwrap_or(&DEFAULT_COLLAPSED)
    }

    /// Collapse or expand one section of the view on screen, from its
    /// header's chevron or (for Snoozed) the tools-bar pill, and remember it.
    /// A selected PR that collapses away hands the selection to the next PR
    /// still shown, and the scroll stays where it is.
    fn toggle_section(&mut self, kind: SectionKind, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let view = collapse_view(state.mode, state.scope.is_all());
        let kinds = self
            .collapsed
            .entry(view)
            .or_insert_with(|| DEFAULT_COLLAPSED.to_vec());
        if let Some(ix) = kinds.iter().position(|k| *k == kind) {
            kinds.remove(ix);
        } else {
            kinds.push(kind);
        }
        crate::config::persist_collapsed(view, kinds);
        let had_selection = self.table.read(cx).selected_row().is_some();
        self.sync_table(cx);
        // A selection inside the section it folded now rests on the folded
        // header; one on the header it unfolded moves to the section's first
        // PR. Either way `c` again undoes it.
        let lost = had_selection
            && self
                .table
                .read(cx)
                .selected_row()
                .is_none_or(|ix| !self.table.read(cx).delegate().is_selectable(ix));
        if lost {
            let next = {
                let delegate = self.table.read(cx).delegate();
                delegate.header_index(kind).and_then(|header| {
                    let len = delegate.display_len();
                    (header..len)
                        .chain((0..header).rev())
                        .find(|&ix| delegate.is_selectable(ix))
                })
            };
            if let Some(ix) = next {
                self.last_selected = ix;
                self.table.update(cx, |table, cx| {
                    table.delegate().set_click_intent(ix, ClickIntent::Keep);
                    table.set_selected_row(ix, cx);
                    table.scroll_to_row(ix, cx);
                });
            }
        }
        cx.notify();
    }

    /// `c`: fold the selected PR's section, or unfold the selected folded
    /// section.
    fn toggle_selected_section(&mut self, cx: &mut Context<Self>) {
        let kind = {
            let table = self.table.read(cx);
            table.selected_row().and_then(|ix| {
                let delegate = table.delegate();
                delegate
                    .folded_section_at(ix)
                    .or_else(|| delegate.section_at(ix))
            })
        };
        if let Some(kind) = kind.filter(|kind| COLLAPSIBLE_SECTIONS.contains(kind)) {
            self.toggle_section(kind, cx);
        }
    }

    /// Whether the search holds the `is:stale` chip: the Stale pill is on.
    fn stale_filtering(&self) -> bool {
        let chip = stale_chip();
        self.filter_chips.iter().any(|held| held.same_as(&chip))
    }

    /// The Stale pill adds the `is:stale` chip, or removes it, as if typed.
    fn toggle_stale_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let chip = stale_chip();
        if let Some(index) = self
            .filter_chips
            .iter()
            .position(|held| held.same_as(&chip))
        {
            self.remove_filter_chip(index, window, cx);
        } else {
            self.filter_by(chip, window, cx);
        }
    }

    /// Whether the search holds the `is:agent` chip: the Agents pill is on.
    fn agent_filtering(&self) -> bool {
        let chip = agent_chip();
        self.filter_chips.iter().any(|held| held.same_as(&chip))
    }

    /// The Agents pill adds the `is:agent` chip, or removes it, as if typed.
    fn toggle_agent_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let chip = agent_chip();
        if let Some(index) = self
            .filter_chips
            .iter()
            .position(|held| held.same_as(&chip))
        {
            self.remove_filter_chip(index, window, cx);
        } else {
            self.filter_by(chip, window, cx);
        }
    }

    /// A label, author, or repository was clicked: filter by it, show the
    /// search box, and keep the keyboard on the board.
    fn filter_by(&mut self, chip: FilterChip, window: &mut Window, cx: &mut Context<Self>) {
        self.search_open = true;
        self.add_filter_chip(chip, cx);
        self.sync_table(cx);
        self.table.focus_handle(cx).focus(window, cx);
    }

    fn sync_table(&mut self, cx: &mut Context<Self>) {
        #[cfg(feature = "perf")]
        let _timer = crate::perf::RebuildTimer::start();
        // All open asks GitHub for the label and author chips. Only chips
        // count, and they change on a click or a finished term, never on a
        // keystroke; unchanged chips cost nothing here.
        let remote = RemoteFilter::from_chips(&self.filter_chips);
        self.state
            .update(cx, |state, cx| state.set_remote_filter(remote, cx));
        let state = self.state.read(cx);
        let stale = StaleRule {
            now: Utc::now(),
            after_days: state.config.stale_after_days,
        };
        // The stores are scanned once here, not once per row.
        let marks = state.marks();
        let search = Search::new(&self.filter_text, &self.filter_chips);
        // The rows and the pills' numbers by core's rules (`query_board`).
        let query = BoardQuery {
            search: &search,
            stale,
            changed_only: self.changed_only,
            needs_you_only: self.needs_you_only,
        };
        let (shown, counts) = query_board(&state.rows, &query, &marks);
        self.counts = counts;
        let rows: Vec<BoardRow> = shown.into_iter().cloned().collect();
        let mut attention = RowAttention {
            collapsed: self.collapsed_here(state).to_vec(),
            ..RowAttention::default()
        };
        for row in &rows {
            if marks.is_changed(&row.id) {
                let summary = state.change_summary(&row.id);
                attention
                    .changed
                    .insert(row.id.clone(), changed_marker_tooltip(&summary));
            }
            if marks.is_watched(&row.id) {
                attention.watched.insert(row.id.clone());
            }
            if marks.is_snoozed(&row.id) {
                attention.snoozed.insert(row.id.clone());
            }
        }
        self.visible_count = rows.len();
        let can_load_more = state.pagination_can_load_more();
        let switching = self.pending_restore.take();
        let target_url = match switching {
            Some(mode) => self.selections.get(&mode).cloned(),
            None => self.selected_row_url(cx),
        };
        // A selected folded header stays selected while it stays folded.
        let target_header = match switching {
            Some(_) => None,
            None => {
                let table = self.table.read(cx);
                table
                    .selected_row()
                    .and_then(|ix| table.delegate().folded_section_at(ix))
            }
        };
        self.table.update(cx, |table, cx| {
            table.delegate_mut().set_stale_after_days(stale.after_days);
            table.delegate_mut().set_sort(if self.smallest_first {
                Sort::Smallest
            } else {
                Sort::Wait
            });
            table
                .delegate_mut()
                .set_section_order(self.section_order.clone());
            table.delegate_mut().set_can_load_more(can_load_more);
            table.delegate_mut().set_rows_and_attention(rows, attention);
            table.refresh(cx);
            if let Some(ix) = target_url.and_then(|u| table.delegate().display_index_of_url(&u)) {
                self.suppress_ack_for = table.delegate().row(ix).map(|row| row.id.clone());
                table.delegate().set_click_intent(ix, ClickIntent::Keep);
                table.set_selected_row(ix, cx);
                if switching.is_some() {
                    table.scroll_to_row(ix, cx);
                }
            } else if let Some(ix) = target_header
                .and_then(|kind| table.delegate().header_index(kind))
                .filter(|&ix| table.delegate().is_selectable(ix))
            {
                table.delegate().set_click_intent(ix, ClickIntent::Keep);
                table.set_selected_row(ix, cx);
            } else {
                table.clear_selection(cx);
            }
        });
        if self.table.read(cx).selected_row().is_none() {
            self.set_details_open(false, cx);
        }
        cx.notify();
    }

    fn select_scope(&mut self, scope: BoardScope, cx: &mut Context<Self>) {
        if self.state.read(cx).scope == scope {
            return;
        }
        crate::config::persist_scope(&scope);
        self.selections.clear();
        self.pending_restore = None;
        self.last_selected = 0;
        self.clear_multi_selection(cx);
        self.set_details_open(false, cx);
        let all_repos = scope.is_all();
        self.state.update(cx, |s, cx| s.switch_scope(scope, cx));
        // All repositories has no All open; the state fell back to My PRs.
        let mode = self.state.read(cx).mode;
        let cols = self.columns_for_current(mode, cx);
        self.table.update(cx, |table, cx| {
            table.delegate_mut().set_scope(all_repos);
            table.delegate_mut().set_mode(mode);
            table.delegate_mut().set_columns(cols);
            table.refresh(cx);
        });
    }

    fn toggle_pin(&mut self, cx: &mut Context<Self>) {
        let Some(repo) = self.state.read(cx).scope.repository().map(str::to_owned) else {
            return;
        };
        if let Some(ix) = self
            .pinned_repos
            .iter()
            .position(|pin| pin.eq_ignore_ascii_case(&repo))
        {
            self.pinned_repos.remove(ix);
        } else if self.pinned_repos.len() < crate::config::MAX_PINNED_REPOS {
            self.pinned_repos.push(repo);
        }
        crate::config::persist_pins(&self.pinned_repos);
        cx.notify();
    }

    fn show_config(&self, window: &mut Window, cx: &mut Context<Self>) {
        let live = {
            let state = self.state.read(cx);
            state.signed_in_via.zip(state.me.clone())
        };
        let settings = cx.new(|cx| crate::settings::SettingsView::new(live, window, cx));
        cx.subscribe_in(
            &settings,
            window,
            |this, _, _: &crate::settings::SettingsSaved, window, cx| {
                // Settings refuses to save into a file it can't read, so a
                // save means the file reads now; its values are checked again.
                let (file, config_warnings) = crate::config::ConfigWarnings::load();
                this.config_warnings = config_warnings;
                let was_enabled = this.updates.automatic;
                this.updates.automatic = file.automatic_update_checks;
                this.theme_pref = ThemePref::resolve(file.theme.as_deref());
                this.theme_pref.apply(window, cx);
                // The order is the window's alone, so a change to it redraws
                // the rows already here rather than waiting for a fetch.
                let sections = prmarmot_local::config::section_order(&file);
                let collapsed = collapsed_from(&file);
                if this.section_order != sections || this.collapsed != collapsed {
                    this.section_order = sections;
                    this.collapsed = collapsed;
                    this.sync_table(cx);
                }
                let position = prmarmot_local::config::details_position(&file);
                if this.details_position != position {
                    this.details_position = position;
                    this.fit_columns(cx);
                }
                let refresh = crate::state::refresh_interval(file.refresh_secs);
                if this.refresh != refresh {
                    this.refresh = refresh;
                    this.start_refresh_loop(cx);
                }
                this.state.update(cx, |state, cx| {
                    state.apply_attention_preferences(
                        crate::state::AttentionPreferences::from_file(&file),
                    );
                    state.apply_config(crate::board_config(&file), cx)
                });
                window.close_dialog(cx);
                this.table.focus_handle(cx).focus(window, cx);
                this.show_feedback("Settings saved", cx);
                if file.notifications {
                    this.notification_help_shown = false;
                    this.state.read(cx).check_notification_permission();
                }
                if !was_enabled && this.updates.automatic {
                    // The checker owns the persisted daily gate. Re-enabling
                    // evaluates it immediately without bypassing that limit.
                    this.check_for_updates(cx);
                }
            },
        )
        .detach();
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title("Settings")
                .w(px(560.))
                .close_button(false)
                .child(settings.clone())
        });
    }

    /// One replaceable confirmation, with no queue or continuous animation.
    fn show_feedback(&mut self, message: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.feedback = Some(message.into());
        self.feedback_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(3)).await;
            let _ = this.update(cx, |this, cx| {
                this.feedback = None;
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// The empty board's button.
    fn empty_action(
        &mut self,
        action: prmarmot_core::status::EmptyAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            prmarmot_core::status::EmptyAction::ShowAllRepositories => {
                self.select_scope(BoardScope::AllRepositories, cx);
                self.repo_select.update(cx, |select, cx| {
                    select.set_selected_value(&ALL_REPOS_LABEL.to_owned(), window, cx);
                });
            }
            prmarmot_core::status::EmptyAction::LoadMore => {
                self.state.update(cx, |state, cx| state.load_more(cx));
            }
        }
        self.table.focus_handle(cx).focus(window, cx);
    }

    /// The empty body when the search matched nothing. All open's is exact
    /// when GitHub answered all of it for these rows, or when nothing is left
    /// to load.
    fn no_match_text(&self, cx: &App) -> String {
        let s = self.state.read(cx);
        let exact = s.mode == Mode::AllOpen
            && (!s.truncated
                || local_only_terms(&self.filter_text, &self.filter_chips, &s.rows_filter)
                    .is_empty());
        if exact {
            all_open_no_match_text(&self.filter_summary())
        } else {
            no_loaded_match_text(&self.filter_summary())
        }
    }

    fn discover_repos(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.discovering_repos {
            return;
        }
        self.repos_discovered = true;
        self.discovering_repos = true;
        self.repo_status = "Finding accessible repositories…".into();
        cx.notify();
        let connector = self.state.read(cx).connector();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { connector.list_repos() })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.discovering_repos = false;
                match result {
                    Ok(discovery) => {
                        let current = scope_label(&this.state.read(cx).scope);
                        let mut repos = this.configured_repos.clone();
                        repos.extend(discovery.repos);
                        if current != ALL_REPOS_LABEL {
                            repos.push(current.clone());
                        }
                        repos.sort_unstable_by_key(|r| r.to_lowercase());
                        repos.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
                        this.repo_status = repo_count_status(repos.len(), discovery.truncated);
                        repos.insert(0, ALL_REPOS_LABEL.to_owned());
                        this.repo_select.update(cx, |select, cx| {
                            select.set_items(SearchableVec::new(repos), window, cx);
                            select.set_selected_value(&current, window, cx);
                            cx.notify();
                        });
                    }
                    Err(err) => {
                        this.repo_status =
                            format!("Repo discovery failed: {err} · retry with Repos")
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn start_refresh_loop(&mut self, cx: &mut Context<Self>) {
        let interval = self.refresh;
        let state = self.state.downgrade();
        // Replacing the task cancels the old timer, leaving exactly one loop.
        // It sleeps until the open view's last refresh is `interval` old, so
        // a view switch or a manual refresh moves the next one back.
        self.refresh_task = Some(cx.spawn(async move |_this, cx| loop {
            let Ok(wait) = state.read_with(cx, |s, _| s.refresh_wait(interval)) else {
                break; // app is shutting down
            };
            cx.background_executor().timer(wait).await;
            // Rate-limit gating and in-flight dedup live inside refresh().
            if state
                .update(cx, |s, cx| s.refresh_if_due(interval, cx))
                .is_err()
            {
                break;
            }
        }));
    }

    fn selected_row_url(&self, cx: &App) -> Option<String> {
        let table = self.table.read(cx);
        let row_ix = table.selected_row()?;
        table.delegate().row(row_ix).map(|r| r.url.clone())
    }

    fn selected_row_id(&self, cx: &App) -> Option<String> {
        let table = self.table.read(cx);
        table
            .selected_row()
            .and_then(|row_ix| table.delegate().row(row_ix))
            .map(|row| row.id.clone())
    }

    fn open_row(&self, row_ix: usize, cx: &mut Context<Self>) {
        if let Some(row) = self.table.read(cx).delegate().row(row_ix) {
            cx.open_url(&row.url.clone());
        }
    }

    /// Switch the visible queue from either mouse or keyboard, keeping the
    /// data query, column set, and persisted preference in lockstep. The
    /// outgoing queue's selection is remembered and the incoming queue's is
    /// restored by the generation observer once its rows land.
    fn select_mode(&mut self, mode: Mode, cx: &mut Context<Self>) {
        let current = self.state.read(cx).mode;
        if current == mode {
            return;
        }
        if mode == Mode::AllOpen && self.state.read(cx).scope.is_all() {
            self.show_feedback(all_open_needs_repository(), cx);
            return;
        }
        // Remember the PR we're leaving by URL (stable across the refresh that
        // the target queue will run).
        if let Some(ix) = self.table.read(cx).selected_row() {
            if let Some(url) = self
                .table
                .read(cx)
                .delegate()
                .row(ix)
                .map(|r| r.url.clone())
            {
                self.selections.insert(current, url);
            }
        }
        self.last_selected = 0;
        self.clear_multi_selection(cx);
        self.pending_restore = Some(mode);
        self.state.update(cx, |s, cx| s.set_mode(mode, cx));
        // The delegate no longer owns column widths (they track the live window
        // width); rebuild them here for the new queue, honoring any manual
        // override stored for this (queue, width class).
        let cols = self.columns_for_current(mode, cx);
        self.table.update(cx, |table, cx| {
            table.delegate_mut().set_mode(mode);
            table.delegate_mut().set_columns(cols);
            table.refresh(cx);
        });
        crate::config::persist_str(
            "view",
            match mode {
                Mode::Authored => "authored",
                Mode::Review => "review",
                Mode::AllOpen => "all",
            },
        );
    }

    /// The columns for `mode` at the current width class: a stored manual
    /// override if one is present (and still matches the column count), else the
    /// responsive default for this window width.
    fn columns_for_current(&self, mode: Mode, cx: &App) -> Vec<Column> {
        let all_repos = self.state.read(cx).scope.is_all();
        let mut cols = columns_for(mode, self.width_class, self.table_width(), all_repos);
        if let Some(widths) = self.col_overrides.get(&(mode, self.width_class, all_repos)) {
            if widths.len() == cols.len() {
                for (col, w) in cols.iter_mut().zip(widths) {
                    col.width = *w;
                }
            }
        }
        cols
    }

    /// Recompute the responsive layout on a window resize. A manual layout is
    /// respected while the width class is unchanged; crossing into a different
    /// class applies that class's stored override or its responsive default. A
    /// no-op rebuild (the quantized widths didn't move) is skipped so a resize
    /// drag doesn't thrash the table.
    fn relayout(&mut self, window: &Window, cx: &mut Context<Self>) {
        self.viewport_width = window.viewport_size().width.into();
        self.fit_columns(cx);
    }

    /// Open or close Details. A right-hand panel takes its width from the
    /// table, so the columns are laid out again.
    fn set_details_open(&mut self, open: bool, cx: &mut Context<Self>) {
        if self.details_open != open {
            self.details_open = open;
            self.fit_columns(cx);
        }
    }

    /// Whether Details sits on the right: always for `right`, never for
    /// `bottom`, and for `auto` when the table keeps a Medium or Wide layout
    /// beside it.
    fn details_on_right(&self) -> bool {
        match self.details_position {
            DetailsPosition::Right => true,
            DetailsPosition::Bottom => false,
            DetailsPosition::Auto => {
                self.viewport_width - DETAILS_PANEL_WIDTH >= crate::table::COMPACT_MAX
            }
        }
    }

    /// The width the table has: the window's, less an open right-hand panel.
    fn table_width(&self) -> f32 {
        if self.details_open && self.details_on_right() {
            (self.viewport_width - DETAILS_PANEL_WIDTH).max(0.)
        } else {
            self.viewport_width
        }
    }

    /// Lay the columns out for the table's width: its width class, and the
    /// flexible Title and Note, follow the table rather than the window.
    fn fit_columns(&mut self, cx: &mut Context<Self>) {
        let width = self.table_width();
        let new_class = TableWidthClass::from_width(width);
        let class_changed = new_class != self.width_class;
        self.width_class = new_class;
        let mode = self.state.read(cx).mode;
        let all_repos = self.state.read(cx).scope.is_all();
        if !class_changed
            && self
                .col_overrides
                .contains_key(&(mode, new_class, all_repos))
        {
            return; // manual layout stands within its width class
        }
        let cols = self.columns_for_current(mode, cx);
        let next_widths: Vec<Pixels> = cols.iter().map(|c| c.width).collect();
        self.table.update(cx, |table, cx| {
            if table.delegate().column_widths() == next_widths {
                return;
            }
            table.delegate_mut().set_columns(cols);
            table.refresh(cx);
        });
    }

    /// A manual column drag finished: write the widths through to the delegate
    /// (so the next fetch's `refresh` preserves them, fixing the reset-on-fetch
    /// bug) and remember them for this (queue, width class). A width vector that
    /// doesn't match the active columns — a mode-switch race — is ignored.
    fn on_column_widths_changed(&mut self, widths: Vec<Pixels>, cx: &mut Context<Self>) {
        let mode = self.state.read(cx).mode;
        let applied = self.table.update(cx, |table, _cx| {
            table.delegate_mut().set_column_widths(&widths)
        });
        if applied {
            let all_repos = self.state.read(cx).scope.is_all();
            self.col_overrides
                .insert((mode, self.width_class, all_repos), widths);
        }
    }

    /// Keep keyboard/mouse selection off the section-header pseudo-rows (a
    /// folded section's header excepted, so `c` can unfold it): when one gets
    /// selected, bounce to the nearest selectable row in the direction of
    /// travel (`set_selected_row` re-emits `SelectRow`, but the bounced-to row
    /// is a real PR, so it settles in one hop).
    fn on_select_row(&mut self, row_ix: usize, cx: &mut Context<Self>) {
        let skipped = |i: usize, this: &Self, cx: &Context<Self>| {
            !this.table.read(cx).delegate().is_selectable(i)
        };
        if !skipped(row_ix, self, cx) {
            let intent = self.table.read(cx).delegate().take_click_intent(row_ix);
            let previous = self.last_selected;
            self.last_selected = row_ix;
            self.apply_select_intent(intent, row_ix, previous, cx);
            if let Some(pr_id) = self
                .table
                .read(cx)
                .delegate()
                .row(row_ix)
                .map(|row| row.id.clone())
            {
                if self.suppress_ack_for.as_deref() == Some(&pr_id) {
                    self.suppress_ack_for = None;
                } else {
                    self.state
                        .update(cx, |state, cx| state.acknowledge(&pr_id, cx));
                }
            } else {
                // A folded section header: there is no PR to show.
                self.set_details_open(false, cx);
            }
            cx.notify();
            return;
        }
        let len = self.table.read(cx).delegate().display_len();
        let going_down = row_ix >= self.last_selected;
        let down = (row_ix + 1..len).find(|&i| !skipped(i, self, cx));
        let up = (0..row_ix).rev().find(|&i| !skipped(i, self, cx));
        let target = if going_down { down.or(up) } else { up.or(down) };
        if let Some(t) = target {
            self.last_selected = t;
            self.table.update(cx, |table, cx| {
                table.delegate().set_click_intent(t, ClickIntent::Keep);
                table.set_selected_row(t, cx)
            });
        }
    }

    /// What a select does to the rows already selected: a plain select
    /// leaves one row selected, ⌘ toggles the clicked row, ⇧ selects from
    /// the anchor to it. `previous` is the caret row before this select.
    fn apply_select_intent(
        &mut self,
        intent: ClickIntent,
        row_ix: usize,
        previous: usize,
        cx: &mut Context<Self>,
    ) {
        match intent {
            ClickIntent::Keep => return,
            ClickIntent::Plain => {
                self.selection_anchor = Some(row_ix);
                self.table.update(cx, |table, _| {
                    table.delegate_mut().clear_multi();
                });
            }
            ClickIntent::Range => {
                let anchor = self.selection_anchor.unwrap_or(row_ix);
                self.table.update(cx, |table, _| {
                    let ids = table.delegate().ids_between(anchor, row_ix);
                    table.delegate_mut().set_multi(ids);
                });
            }
            ClickIntent::Toggle => {
                self.selection_anchor = Some(row_ix);
                let still_selected = self.table.update(cx, |table, cx| {
                    let Some(clicked) = table.delegate().row(row_ix).map(|row| row.id.clone())
                    else {
                        return true;
                    };
                    let caret = (previous != row_ix)
                        .then(|| table.delegate().row(previous).map(|row| row.id.clone()))
                        .flatten();
                    if table
                        .delegate_mut()
                        .toggle_multi(&clicked, caret.as_deref())
                    {
                        return true;
                    }
                    // The caret sits on a row that is no longer selected:
                    // park it on the nearest one that is.
                    match table.delegate().nearest_multi_index(row_ix) {
                        Some(ix) => {
                            if table.delegate().multi_len() == 1 {
                                table.delegate_mut().clear_multi();
                            }
                            table.delegate().set_click_intent(ix, ClickIntent::Keep);
                            table.set_selected_row(ix, cx);
                            true
                        }
                        None => {
                            table.clear_selection(cx);
                            false
                        }
                    }
                });
                if !still_selected {
                    self.set_details_open(false, cx);
                }
            }
        }
        cx.notify();
    }

    /// ⇧↑ / ⇧↓: grow the selection by one PR from the caret, from the
    /// anchor, which the first extension sets at the caret.
    fn extend_selection(&mut self, down: bool, cx: &mut Context<Self>) {
        let (caret, target) = {
            let table = self.table.read(cx);
            let delegate = table.delegate();
            let Some(caret) = table.selected_row() else {
                return;
            };
            let target = if down {
                (caret + 1..delegate.display_len()).find(|&i| delegate.row(i).is_some())
            } else {
                (0..caret).rev().find(|&i| delegate.row(i).is_some())
            };
            (caret, target)
        };
        let Some(target) = target else {
            return;
        };
        if self.selection_anchor.is_none() {
            self.selection_anchor = Some(caret);
        }
        self.table.update(cx, |table, cx| {
            table
                .delegate()
                .set_click_intent(target, ClickIntent::Range);
            table.set_selected_row(target, cx);
        });
    }

    /// ⌘A (Ctrl+A): every PR on screen; folded sections stay folded and
    /// out of it.
    fn select_all_rows(&mut self, cx: &mut Context<Self>) {
        self.table.update(cx, |table, cx| {
            let ids = table.delegate().visible_pr_ids();
            table.delegate_mut().set_multi(ids);
            let caret_on_pr = table
                .selected_row()
                .is_some_and(|ix| table.delegate().row(ix).is_some());
            if !caret_on_pr {
                if let Some(ix) = table.delegate().nearest_multi_index(0) {
                    table.delegate().set_click_intent(ix, ClickIntent::Keep);
                    table.set_selected_row(ix, cx);
                }
            }
        });
        cx.notify();
    }

    /// Back to the caret row alone. True when there was more.
    fn clear_multi_selection(&mut self, cx: &mut Context<Self>) -> bool {
        self.selection_anchor = None;
        let cleared = self
            .table
            .update(cx, |table, _| table.delegate_mut().clear_multi());
        if cleared {
            cx.notify();
        }
        cleared
    }

    fn multi_selection_active(&self, cx: &App) -> bool {
        self.table.read(cx).delegate().multi_active()
    }

    /// One action over the whole selection, from its menu or a key.
    fn selection_action(
        &mut self,
        action: crate::table::SelectionAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::table::SelectionAction;
        use prmarmot_core::status::{selection_done_text, SelectionDone, MAX_OPEN_TOGETHER};
        let rows = self.table.read(cx).delegate().multi_rows();
        if rows.is_empty() {
            return;
        }
        let done = match action {
            SelectionAction::Open => {
                let opened = rows.len().min(MAX_OPEN_TOGETHER);
                for row in &rows[..opened] {
                    cx.open_url(&row.url);
                }
                SelectionDone::Opened {
                    opened,
                    selected: rows.len(),
                }
            }
            SelectionAction::Copy(format) => {
                if let Some(copy) = self.table.read(cx).delegate().selection_copy(format) {
                    self.copy_group(copy, cx);
                }
                return;
            }
            SelectionAction::Watch => SelectionDone::Watching(self.watch_rows(&rows, true, cx)),
            SelectionAction::Unwatch => SelectionDone::Unwatched(self.watch_rows(&rows, false, cx)),
            SelectionAction::Snooze => {
                self.snooze_rows(rows, window, cx);
                return;
            }
            SelectionAction::CancelSnooze => {
                let cancelled = self.state.update(cx, |state, cx| {
                    let mut cancelled = 0;
                    for row in &rows {
                        if state.snooze_description(&row.id).is_some() {
                            state.cancel_snooze(&row.id, cx);
                            cancelled += 1;
                        }
                    }
                    cancelled
                });
                SelectionDone::SnoozesCancelled(cancelled)
            }
        };
        self.show_feedback(selection_done_text(done), cx);
    }

    /// Watch (or unwatch) the rows not already so. How many changed.
    fn watch_rows(
        &mut self,
        rows: &[prmarmot_core::board::BoardRow],
        watch: bool,
        cx: &mut Context<Self>,
    ) -> usize {
        self.state.update(cx, |state, cx| {
            let mut changed = 0;
            for row in rows {
                if state.is_watched(&row.id) != watch {
                    state.toggle_watch(row, cx);
                    changed += 1;
                }
            }
            changed
        })
    }

    /// `w` over a selection: watch the ones not watched, or when every one
    /// is, unwatch them all.
    fn toggle_watch_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let any_unwatched = {
            let rows = self.table.read(cx).delegate().multi_rows();
            let state = self.state.read(cx);
            rows.iter().any(|row| !state.is_watched(&row.id))
        };
        let action = if any_unwatched {
            crate::table::SelectionAction::Watch
        } else {
            crate::table::SelectionAction::Unwatch
        };
        self.selection_action(action, window, cx);
    }

    /// The snooze dialog for several PRs: the same choices as one PR's,
    /// each applied to every row (a condition's baseline is the row's own).
    /// "Waiting on" is one person's, so it is not offered here.
    fn snooze_rows(
        &self,
        rows: Vec<prmarmot_core::board::BoardRow>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::attention_state::{AttentionState, SnoozeCondition};
        type Choice = fn(&prmarmot_core::board::BoardRow) -> SnoozeCondition;
        let state = self.state.clone();
        let view = cx.entity().downgrade();
        let focus = self.table.focus_handle(cx);
        let any_snoozed = {
            let state = state.read(cx);
            rows.iter()
                .any(|row| state.snooze_description(&row.id).is_some())
        };
        let count = rows.len();
        window.open_dialog(cx, move |dialog, _, _| {
            let choices: [(&str, Choice); 4] = [
                (crate::attention_state::SNOOZE_ONE_HOUR, |_| {
                    SnoozeCondition::Until {
                        deadline: Utc::now() + ChronoDuration::hours(1),
                    }
                }),
                (crate::attention_state::SNOOZE_UNTIL_TOMORROW, |_| {
                    SnoozeCondition::Until {
                        deadline: Utc::now() + ChronoDuration::hours(24),
                    }
                }),
                (
                    crate::attention_state::SNOOZE_WAITING_CI,
                    AttentionState::waiting_ci,
                ),
                (
                    crate::attention_state::SNOOZE_REVIEW_AGAIN,
                    AttentionState::review_again,
                ),
            ];
            let mut options = v_flex().gap_2();
            for (index, (label, choice)) in choices.into_iter().enumerate() {
                let state = state.clone();
                let view = view.clone();
                let rows = rows.clone();
                options = options.child(
                    Button::new(("snooze-selection", index))
                        .label(label)
                        .on_click(move |_, window, cx| {
                            state.update(cx, |state, cx| {
                                for row in &rows {
                                    state.set_snooze(row, choice(row), cx);
                                }
                            });
                            let _ = view.update(cx, |this, cx| {
                                this.show_feedback(
                                    prmarmot_core::status::selection_done_text(
                                        prmarmot_core::status::SelectionDone::Snoozed(rows.len()),
                                    ),
                                    cx,
                                )
                            });
                            window.close_dialog(cx);
                        }),
                );
            }
            if any_snoozed {
                let state = state.clone();
                let rows = rows.clone();
                options = options.child(
                    Button::new("cancel-selection-snooze")
                        .label(crate::attention_state::SNOOZE_CANCEL)
                        .on_click(move |_, window, cx| {
                            state.update(cx, |state, cx| {
                                for row in &rows {
                                    state.cancel_snooze(&row.id, cx);
                                }
                            });
                            window.close_dialog(cx);
                        }),
                );
            }
            let focus = focus.clone();
            dialog
                .title(format!("Snooze {count} PRs"))
                .w(px(360.))
                .close_button(true)
                .child(options)
                .on_close(move |_, window, cx| focus.focus(window, cx))
        });
    }

    fn handle_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if window.has_active_dialog(cx) {
            return;
        }
        if event.keystroke.key == "escape" {
            // A selection of several rows goes first; Details next.
            if self.clear_multi_selection(cx) {
                return;
            }
            self.set_details_open(false, cx);
            self.table.focus_handle(cx).focus(window, cx);
            cx.notify();
            return;
        }
        // Text inputs own character keys; toolbar buttons do not. The select's
        // dynamic focus handle includes its searchable popup.
        let in_text_input = self.search.focus_handle(cx).contains_focused(window, cx)
            || self
                .repo_select
                .focus_handle(cx)
                .contains_focused(window, cx);
        let table_focused = self.table.focus_handle(cx).contains_focused(window, cx);
        let modifiers = event.keystroke.modifiers;
        // ⌘A (Ctrl+A on Linux) over the table: every PR on screen.
        if !in_text_input && table_focused && modifiers.secondary() && event.keystroke.key == "a" {
            self.select_all_rows(cx);
            cx.stop_propagation();
            return;
        }
        if in_text_input || modifiers.platform || modifiers.control || modifiers.alt {
            return;
        }
        let key = event.keystroke.key.as_str();
        let platform = modifiers.platform;
        // With several rows selected, the row keys act on all of them.
        let multi = self.multi_selection_active(cx);
        match key {
            "down" | "up" if table_focused && modifiers.shift => {
                self.extend_selection(key == "down", cx);
                cx.stop_propagation();
            }
            "enter" if table_focused && multi => {
                self.selection_action(crate::table::SelectionAction::Open, window, cx)
            }
            "o" if multi => self.selection_action(crate::table::SelectionAction::Open, window, cx),
            "y" if !platform && modifiers.shift && multi => self.selection_action(
                crate::table::SelectionAction::Copy(prmarmot_core::share::ShareFormat::List),
                window,
                cx,
            ),
            "y" if !platform && multi => self.selection_action(
                crate::table::SelectionAction::Copy(prmarmot_core::share::ShareFormat::Urls),
                window,
                cx,
            ),
            "w" if !platform && table_focused && multi => self.toggle_watch_selection(window, cx),
            "s" if !platform && table_focused && multi => {
                self.selection_action(crate::table::SelectionAction::Snooze, window, cx)
            }
            "/" if !platform => {
                self.open_search(window, cx);
                cx.stop_propagation();
            }
            "space" if table_focused => self.toggle_details(window, cx),
            "q" => {
                save_window_size(window);
                cx.quit();
            }
            "r" => self.state.update(cx, |s, cx| {
                if s.setup == SetupStatus::Ready {
                    s.refresh(cx)
                } else {
                    s.validate_setup(cx)
                }
            }),
            "v" if !platform => {
                let state = self.state.read(cx);
                let mode = match state.mode {
                    Mode::Authored => Mode::Review,
                    // All open is skipped where it cannot load.
                    Mode::Review if state.scope.is_all() => Mode::Authored,
                    Mode::Review => Mode::AllOpen,
                    Mode::AllOpen => Mode::Authored,
                };
                self.select_mode(mode, cx);
            }
            "1" if !platform => self.select_mode(Mode::Authored, cx),
            "2" if !platform => self.select_mode(Mode::Review, cx),
            "3" if !platform => self.select_mode(Mode::AllOpen, cx),
            "t" if !platform => {
                self.theme_pref = self.theme_pref.next();
                self.theme_pref.apply(window, cx);
                crate::config::persist_str("theme", self.theme_pref.label());
                cx.notify();
            }
            "enter" if table_focused => {
                if let Some(url) = self.selected_row_url(cx) {
                    cx.open_url(&url);
                }
            }
            "o" => {
                if let Some(url) = self.selected_row_url(cx) {
                    cx.open_url(&url);
                }
            }
            "y" if !platform && event.keystroke.modifiers.shift => self.copy_selected_group(cx),
            "y" if !platform => {
                if let Some(url) = self.selected_row_url(cx) {
                    cx.write_to_clipboard(ClipboardItem::new_string(url));
                    self.show_feedback("PR URL copied", cx);
                }
            }
            "w" if !platform && table_focused => self.toggle_watch(cx),
            "s" if !platform && table_focused => self.show_snooze_menu(window, cx),
            "c" if !platform && table_focused => self.toggle_selected_section(cx),
            _ => {}
        }
    }

    fn show_notification_help(
        &self,
        permission: crate::platform::NotificationPermission,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let view = cx.entity().downgrade();
        crate::notification_help::open_notification_help(
            window,
            cx,
            permission,
            move |permission, _, cx| {
                let _ = view.update(cx, |this, cx| {
                    this.notification_help_shown = false;
                    #[cfg(target_os = "macos")]
                    if permission == crate::platform::NotificationPermission::NotDetermined {
                        this.state.read(cx).request_notification_permission();
                        return;
                    }
                    let _ = permission;
                    this.state.read(cx).check_notification_permission();
                });
            },
        );
    }

    /// Write a copied board group: HTML + plain text where the platform
    /// clipboard supports it, plain text otherwise.
    fn copy_group(&mut self, copy: crate::table::GroupCopy, cx: &mut Context<Self>) {
        use prmarmot_core::share::{ShareFormat, SharePayload};
        let SharePayload { html, plain } = copy.payload;
        let rich = html
            .as_deref()
            .is_some_and(|html| crate::platform::write_rich_clipboard(html, &plain));
        if !rich {
            cx.write_to_clipboard(ClipboardItem::new_string(plain));
        }
        let what = match copy.format {
            ShareFormat::Urls => "URL",
            _ => "PR",
        };
        let how = match copy.format {
            ShareFormat::List if rich => " as a list",
            ShareFormat::Table if rich => " as a table",
            ShareFormat::List | ShareFormat::Table => " as text",
            ShareFormat::Markdown => " as Markdown",
            ShareFormat::Urls => "",
        };
        let plural = if copy.count == 1 { "" } else { "s" };
        self.show_feedback(format!("Copied {} {what}{plural}{how}", copy.count), cx);
    }

    /// `Y`: copy the group containing the selected PR as a list, or the
    /// selection when it is several rows.
    fn copy_selected_group(&mut self, cx: &mut Context<Self>) {
        let copy = {
            let table = self.table.read(cx);
            if table.delegate().multi_active() {
                table
                    .delegate()
                    .selection_copy(prmarmot_core::share::ShareFormat::List)
            } else {
                table.selected_row().and_then(|row_ix| {
                    let delegate = table.delegate();
                    let label = delegate.group_label_at(row_ix)?;
                    delegate.group_copy(&label, prmarmot_core::share::ShareFormat::List)
                })
            }
        };
        if let Some(copy) = copy {
            self.copy_group(copy, cx);
        }
    }

    fn row_action(
        &mut self,
        pr_id: &str,
        action: crate::table::RowAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::table::RowAction;
        let Some(row) = self.table.read(cx).delegate().row_by_id(pr_id).cloned() else {
            return; // gone from the board since the click
        };
        match action {
            RowAction::Open => cx.open_url(&row.url),
            RowAction::Copy(text) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                self.show_feedback("Copied to clipboard", cx);
            }
            RowAction::Watch => self.watch_row(&row, cx),
            RowAction::Snooze => self.snooze_row(row, window, cx),
            RowAction::CancelSnooze => self
                .state
                .update(cx, |state, cx| state.cancel_snooze(&row.id, cx)),
            RowAction::Details => {
                let index = self
                    .table
                    .read(cx)
                    .delegate()
                    .display_index_of_url(&row.url);
                if let Some(index) = index {
                    self.table
                        .update(cx, |table, cx| table.set_selected_row(index, cx));
                    self.set_details_open(true, cx);
                    cx.notify();
                }
            }
        }
    }

    fn selected_row(&self, cx: &App) -> Option<prmarmot_core::board::BoardRow> {
        let table = self.table.read(cx);
        table
            .selected_row()
            .and_then(|index| table.delegate().row(index))
            .cloned()
    }

    fn toggle_watch(&mut self, cx: &mut Context<Self>) {
        let Some(row) = self.selected_row(cx) else {
            return;
        };
        self.watch_row(&row, cx);
    }

    fn watch_row(&mut self, row: &prmarmot_core::board::BoardRow, cx: &mut Context<Self>) {
        if let Some(message) = self
            .state
            .update(cx, |state, cx| state.toggle_watch(row, cx))
        {
            // The watch bumped the state's generation; its observer syncs
            // the table.
            self.show_feedback_owned(message, cx);
        }
    }

    fn show_feedback_owned(&mut self, message: String, cx: &mut Context<Self>) {
        self.feedback = None;
        self.repo_status = message;
        cx.notify();
    }

    fn show_snooze_menu(&self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.selected_row(cx) else {
            return;
        };
        self.snooze_row(row, window, cx);
    }

    fn snooze_row(
        &self,
        row: prmarmot_core::board::BoardRow,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let state = self.state.clone();
        let focus = self.table.focus_handle(cx);
        let already = state.read(cx).snooze_description(&row.id).is_some();
        let waiting_on_author = state
            .read(cx)
            .me
            .as_deref()
            .and_then(|me| crate::attention_state::AttentionState::waiting_on_author(&row, me));
        window.open_dialog(cx, move |dialog, _, cx| {
            let _ = cx;
            let mut options = v_flex().gap_2();
            let choices = [
                (
                    crate::attention_state::SNOOZE_ONE_HOUR,
                    crate::attention_state::SnoozeCondition::Until {
                        deadline: Utc::now() + ChronoDuration::hours(1),
                    },
                ),
                (
                    crate::attention_state::SNOOZE_UNTIL_TOMORROW,
                    crate::attention_state::SnoozeCondition::Until {
                        deadline: Utc::now() + ChronoDuration::hours(24),
                    },
                ),
                (
                    crate::attention_state::SNOOZE_WAITING_CI,
                    crate::attention_state::AttentionState::waiting_ci(&row),
                ),
                (
                    crate::attention_state::SNOOZE_REVIEW_AGAIN,
                    crate::attention_state::AttentionState::review_again(&row),
                ),
            ];
            for (index, (label, condition)) in choices.into_iter().enumerate() {
                let state = state.clone();
                let row = row.clone();
                options =
                    options.child(Button::new(("snooze-choice", index)).label(label).on_click(
                        move |_, window, cx| {
                            state.update(cx, |state, cx| {
                                state.set_snooze(&row, condition.clone(), cx)
                            });
                            window.close_dialog(cx);
                        },
                    ));
            }
            if let Some(waiting) = waiting_on_author.clone() {
                let state = state.clone();
                let row = row.clone();
                options = options.child(
                    Button::new("snooze-person")
                        .label(
                            crate::attention_state::AttentionState::snooze_for(
                                &row,
                                waiting.clone(),
                            )
                            .description(crate::table::local_offset_secs()),
                        )
                        .on_click(move |_, window, cx| {
                            state.update(cx, |state, cx| {
                                state.set_snooze(&row, waiting.clone(), cx)
                            });
                            window.close_dialog(cx);
                        }),
                );
            }
            if already {
                let state = state.clone();
                let id = row.id.clone();
                options = options.child(
                    Button::new("cancel-snooze")
                        .label(crate::attention_state::SNOOZE_CANCEL)
                        .on_click(move |_, window, cx| {
                            state.update(cx, |state, cx| state.cancel_snooze(&id, cx));
                            window.close_dialog(cx);
                        }),
                );
            }
            let focus = focus.clone();
            dialog
                .title(format!("Snooze #{}", row.number))
                .w(px(360.))
                .close_button(true)
                .child(options)
                .on_close(move |_, window, cx| focus.focus(window, cx))
        });
    }

    /// Await one delivered notification's click as a task, keeping at most
    /// [`crate::platform::MAX_NOTIFICATION_WAITERS`]: past that the oldest
    /// wait is dropped, so a long session of ignored notifications never
    /// stops new ones from being delivered or clicked.
    #[cfg(target_os = "macos")]
    fn await_notification_click(
        &mut self,
        pending: crate::platform::PendingClick,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        while self.notification_clicks.len() >= crate::platform::MAX_NOTIFICATION_WAITERS {
            self.notification_clicks.pop_front();
        }
        let wait = cx.spawn_in(window, async move |this, cx| {
            if let Some((pr_id, url)) = pending.opened().await {
                let _ = this.update_in(cx, |this, window, cx| {
                    window.activate_window();
                    this.select_notification_pr(pr_id, url, cx);
                });
            }
        });
        self.notification_clicks.push_back(wait);
    }

    fn select_notification_pr(&mut self, pr_id: String, url: String, cx: &mut Context<Self>) {
        self.sync_table(cx);
        if let Some(index) = self.table.read(cx).delegate().display_index_of_url(&url) {
            self.table.update(cx, |table, cx| {
                table.set_selected_row(index, cx);
                table.scroll_to_row(index, cx);
            });
            return;
        }
        let watched = self.state.read(cx).watched_fallback(&pr_id);
        let (url, feedback) = crate::state::off_board_click(url, watched);
        cx.open_url(&url);
        self.show_feedback_owned(feedback, cx);
    }

    fn toggle_details(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.details_open {
            self.set_details_open(false, cx);
        } else {
            if self.selected_row_url(cx).is_none() {
                self.table.update(cx, |table, cx| {
                    let first = (0..table.delegate().display_len())
                        .find(|&ix| table.delegate().row(ix).is_some());
                    if let Some(ix) = first {
                        table.set_selected_row(ix, cx);
                        table.scroll_to_row(ix, cx);
                    }
                });
            }
            let open = self.selected_row_url(cx).is_some();
            self.set_details_open(open, cx);
        }
        self.table.focus_handle(cx).focus(window, cx);
        cx.notify();
    }
}

// The header sentence, the toggle tooltips and the two duration phrasings
// live in `prmarmot_core::status`, so the iPad shows the same words.
use prmarmot_core::status::{
    agent_toggle_tooltip, changed_toggle_tooltip, header_counts as core_header_counts,
    loaded_more_text, needs_you_toggle_tooltip, queue_loading_text,
    queue_sync_text as core_queue_sync_text, relative, snoozed_toggle_tooltip,
    stale_toggle_tooltip, BadgeName, HeaderCounts, AGENT_TOGGLE_LABEL, NEEDS_YOU_TOGGLE_LABEL,
    REFRESH_NOTE, STALE_TOGGLE_LABEL, SYNC_STATUS_NOTE,
};

/// The `is:stale` chip the Stale quick filter adds and removes.
fn stale_chip() -> FilterChip {
    FilterChip::new(Qualifier::Is, "stale")
}

/// The `is:agent` chip the Agents quick filter adds and removes.
fn agent_chip() -> FilterChip {
    FilterChip::new(Qualifier::Is, "agent")
}

/// What the table area shows, derived from `AppState` truth (`last_synced` /
/// back-off / error) rather than the row `generation` — so an unseen queue or a
/// first-fetch failure never renders a contradictory "empty" or "Loading…".
enum BodyState {
    Loaded,
    Setup(SetupStatus),
    Loading(String),
    Paused(String),
    Failed(String),
}

fn check_result(check: &AutomaticCheck) -> Option<&CheckResult> {
    match check {
        AutomaticCheck::NotDue(result) => result.as_ref(),
        AutomaticCheck::Completed { result, .. } => Some(result),
        AutomaticCheck::Disabled | AutomaticCheck::InProgressElsewhere => None,
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs() as i64)
}

/// "45s" / "3m" / "1h 5m" — compact, for the back-off retry countdown.
impl RootView {
    /// The board table, its delegate calling back into this view by handle.
    fn build_table(
        mode: Mode,
        all_repos: bool,
        initial_class: TableWidthClass,
        initial_width: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<TableState<BoardTableDelegate>> {
        let view = cx.entity().downgrade();
        cx.new(|cx| {
            let mut delegate = BoardTableDelegate::new(mode, all_repos);
            let group_view = view.clone();
            let empty_view = view.clone();
            let filter_view = view.clone();
            let section_view = view.clone();
            delegate.on_section_toggle = Some(std::rc::Rc::new(move |kind, _, cx| {
                let _ = section_view.update(cx, |this, cx| this.toggle_section(kind, cx));
            }));
            delegate.on_filter_click = Some(std::rc::Rc::new(move |chip, window, cx| {
                let _ = filter_view.update(cx, |this, cx| this.filter_by(chip, window, cx));
            }));
            let selection_view = view.clone();
            delegate.on_row_action = Some(std::rc::Rc::new(move |pr_id, action, window, cx| {
                let _ = view.update(cx, |this, cx| this.row_action(&pr_id, action, window, cx));
            }));
            delegate.on_selection_action = Some(std::rc::Rc::new(move |action, window, cx| {
                let _ =
                    selection_view.update(cx, |this, cx| this.selection_action(action, window, cx));
            }));
            delegate.on_group_copy = Some(std::rc::Rc::new(move |copy, _, cx| {
                let _ = group_view.update(cx, |this, cx| this.copy_group(copy, cx));
            }));
            delegate.on_empty_action = Some(std::rc::Rc::new(move |action, window, cx| {
                let _ = empty_view.update(cx, |this, cx| this.empty_action(action, window, cx));
            }));
            delegate.set_columns(columns_for(mode, initial_class, initial_width, all_repos));
            TableState::new(delegate, window, cx)
                .sortable(false)
                .col_movable(false)
                .col_resizable(true)
                .row_selectable(true)
        })
    }

    /// The repository picker: All repositories first, then the configured
    /// and pinned ones; discovery fills in the rest when it first opens.
    fn build_repo_select(
        state: &Entity<AppState>,
        repos: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<SelectState<SearchableVec<String>>> {
        let current = scope_label(&state.read(cx).scope);
        let mut picker_items = repos;
        picker_items.retain(|repo| !repo.eq_ignore_ascii_case(ALL_REPOS_LABEL));
        picker_items.insert(0, ALL_REPOS_LABEL.to_owned());
        let selected = picker_items
            .iter()
            .position(|r| *r == current)
            .map(IndexPath::new);
        let select = cx.new(|cx| {
            SelectState::new(SearchableVec::new(picker_items), selected, window, cx)
                .searchable(true)
        });
        cx.subscribe_in(
            &select,
            window,
            |this: &mut Self, _, event: &SelectEvent<SearchableVec<String>>, window, cx| {
                let SelectEvent::Confirm(Some(repo)) = event else {
                    return;
                };
                this.select_scope(scope_from_label(repo), cx);
                // The picker focuses itself after it confirms; give the
                // keyboard back to the board once it has, as the pinned
                // repository chips do, or Space and the arrows do nothing.
                cx.defer_in(window, |this, window, cx| {
                    this.table.focus_handle(cx).focus(window, cx);
                });
            },
        )
        .detach();
        // 0.6 renders the trigger from the filtered cursor, not the committed
        // value. Restore the full list on close (Escape or outside click).
        // Focusable exposes the popup handle while open, the trigger otherwise.
        let trigger = select.focus_handle(cx);
        let mut was_open = false;
        cx.observe_in(
            &select,
            window,
            move |this: &mut Self, select, window, cx| {
                let open = select.focus_handle(cx) != trigger;
                let closed = was_open && !open;
                if open && !was_open && !this.repos_discovered {
                    this.discover_repos(window, cx);
                }
                was_open = open;
                if let Some(current) = select.read(cx).selected_value().cloned().filter(|_| closed)
                {
                    select.update(cx, |select, cx| {
                        select.set_selected_value(&current, window, cx);
                        cx.notify();
                    });
                }
            },
        )
        .detach();
        select
    }

    /// The search box: a finished `label:x` becomes a chip, the rest filters.
    fn build_search(mode: Mode, window: &mut Window, cx: &mut Context<Self>) -> Entity<InputState> {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder(search_placeholder(mode)));
        cx.subscribe_in(
            &search,
            window,
            |this: &mut Self, input, event: &InputEvent, window, cx| {
                let all = match event {
                    InputEvent::Change => false,
                    InputEvent::PressEnter { .. } => true,
                    // An empty box closes when you leave it; `/` reopens it.
                    InputEvent::Blur => {
                        if !this.filtering() {
                            this.search_open = false;
                            cx.notify();
                        }
                        return;
                    }
                    _ => return,
                };
                // A finished `label:x` becomes a chip; the field keeps the words.
                let text = input.read(cx).value().to_string();
                let (chips, rest) = take_filter_chips(&text, all);
                if !chips.is_empty() {
                    for chip in chips {
                        this.add_filter_chip(chip, cx);
                    }
                    input.update(cx, |input, cx| input.set_value(rest.clone(), window, cx));
                }
                this.filter_text = rest;
                this.sync_table(cx);
            },
        )
        .detach();
        search
    }

    /// Push new rows into the table only when a fetch actually landed —
    /// gate the observer on generation (the PRFlow infinite-observer trap).
    /// On a mode switch, restore that queue's remembered selection + scroll
    /// once its rows are in place; on a plain refresh, keep the current
    /// selection but clamp it if the row count shrank.
    fn observe_state(state: &Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) {
        cx.observe_in(state, window, |this: &mut Self, state, window, cx| {
            // The view can change without a window at hand (a scope change
            // falls back from All open), so the placeholder follows it here.
            let mode = state.read(cx).mode;
            if this.placeholder_mode != mode {
                this.placeholder_mode = mode;
                this.search.update(cx, |input, cx| {
                    input.set_placeholder(search_placeholder(mode), window, cx)
                });
            }
            // Discovery waits for the first board (or for the picker to
            // open) rather than competing with it for the connection.
            if !this.repos_discovered && state.read(cx).first_fetch_done() {
                this.discover_repos(window, cx);
            }
            let generation = state.read(cx).generation;
            if generation != this.seen_generation {
                this.seen_generation = generation;
                this.sync_table(cx);
                // Load more's rows join their sections, not the bottom.
                if let Some((_, added)) = state
                    .read(cx)
                    .loaded_more
                    .filter(|&(landed, _)| landed == generation)
                {
                    this.show_feedback(loaded_more_text(added), cx);
                }
            }
            cx.notify();
        })
        .detach();
    }
}

impl Focusable for RootView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for RootView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let dialog_layer = gpui_component::Root::render_dialog_layer(window, cx);
        let theme = cx.theme();
        // Body state from truth, not `generation` (which also bumps on a switch
        // to an unseen queue and on a repo change): a queue has loaded ONLY
        // once a fetch succeeded (`last_synced`). Otherwise show the real
        // reason it's not showing rows — paused for back-off, a first-fetch
        // error, or still loading — never a stale "Loading…" over an error or
        // a premature "empty" over a fetch in flight (critique #6).
        let body = {
            let s = self.state.read(cx);
            if s.setup != SetupStatus::Ready {
                BodyState::Setup(s.setup.clone())
            } else if s.last_synced.is_some() {
                BodyState::Loaded
            } else if let Some(paused) = s.pause_text() {
                BodyState::Paused(paused)
            } else if let Some(err) = s.error.clone() {
                BodyState::Failed(format!("Couldn't load — {err}"))
            } else {
                BodyState::Loading(queue_loading_text(s.mode, s.scope.is_all()).to_string())
            }
        };
        let details_right = self.details_open && self.details_on_right();

        v_flex()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .track_focus(&self.focus_handle)
            .when(self.details_open, |view| {
                view.key_context("PrmarmotDetails")
            })
            .on_action(cx.listener(|this, _: &CloseDetails, window, cx| {
                this.set_details_open(false, cx);
                this.table.focus_handle(cx).focus(window, cx);
                cx.notify();
            }))
            // Backspace in an empty search box removes the last chip.
            .capture_action(cx.listener(
                |this, _: &gpui_component::input::Backspace, window, cx| {
                    if this.filter_text.is_empty()
                        && !this.filter_chips.is_empty()
                        && this.search.focus_handle(cx).is_focused(window)
                    {
                        this.filter_chips.pop();
                        this.sync_table(cx);
                        cx.stop_propagation();
                    }
                },
            ))
            .on_action(cx.listener(|this, _: &FocusSearch, window, cx| {
                if !window.has_active_dialog(cx) {
                    this.open_search(window, cx);
                }
            }))
            .on_key_down(cx.listener(Self::handle_key_down))
            .child(TitleBar::new().child(self.render_header(cx)))
            .child(self.render_update_banners(cx))
            .child(self.render_tools(cx))
            // Full-bleed table (spec §5): the window IS the table; 13 px
            // cells at Size::Small density. A right-hand Details panel sits
            // beside it and takes its width from the table's.
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .items_start()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .text_size(type_size::BODY)
                            .map(|this| match body {
                                // bordered defaults to TRUE — at full bleed the outer
                                // border + rounded corners fight the window edge
                                // (spec §5: header border is the only separator). Once
                                // loaded, the delegate's own render_empty shows the
                                // queue-specific "nothing here" — correct, because we
                                // now KNOW the queue is empty.
                                BodyState::Loaded
                                    if self.visible_count == 0 && self.filtering() =>
                                {
                                    this.child(
                                        h_flex()
                                            .size_full()
                                            .justify_center()
                                            .text_color(theme.muted_foreground)
                                            .child(self.no_match_text(cx)),
                                    )
                                }
                                BodyState::Loaded => this.child(
                                    DataTable::new(&self.table)
                                        .small()
                                        .stripe(false)
                                        .bordered(false),
                                ),
                                BodyState::Setup(setup) => this.child(self.render_setup(setup, cx)),
                                BodyState::Loading(text) | BodyState::Paused(text) => this.child(
                                    h_flex()
                                        .size_full()
                                        .justify_center()
                                        .text_color(theme.muted_foreground)
                                        .child(text),
                                ),
                                BodyState::Failed(text) => this.child(
                                    v_flex()
                                        .size_full()
                                        .gap_3()
                                        .items_center()
                                        .justify_center()
                                        .px_4()
                                        .child(
                                            div()
                                                .max_w(px(640.))
                                                .text_color(theme.danger)
                                                .child(text),
                                        )
                                        .child(
                                            Button::new("retry-board")
                                                .label("Retry")
                                                .tooltip("Retry loading this queue · r")
                                                .on_click(cx.listener(|this, _, window, cx| {
                                                    this.state
                                                        .update(cx, |state, cx| state.refresh(cx));
                                                    this.focus_handle.focus(window, cx);
                                                })),
                                        ),
                                ),
                            }),
                    )
                    .when(details_right, |row| {
                        row.child(self.render_details(true, cx))
                    }),
            )
            .when(self.details_open && !details_right, |this| {
                this.child(self.render_details(false, cx))
            })
            .child(self.render_footer(cx))
            .children(dialog_layer)
    }
}

#[cfg(test)]
mod tests {
    use super::repo_count_status;

    #[test]
    fn the_repository_count_leaves_out_all_repositories_and_says_one_repository() {
        assert_eq!(repo_count_status(1, false), "1 repository");
        assert_eq!(repo_count_status(3, false), "3 repositories");
        assert_eq!(
            repo_count_status(1000, true),
            "1000 repositories · discovery limit reached"
        );
    }
}
