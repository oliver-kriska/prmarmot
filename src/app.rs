//! Root view: header (repo, counts, sync + rate-limit status) over the board
//! table, the auto-refresh loop, and the keyboard/mouse actions.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;

use chrono::{DateTime, Duration as ChronoDuration, Local, Utc};
use gpui::prelude::FluentBuilder;
use gpui::{
    div, px, App, AppContext, ClipboardItem, Context, Entity, FocusHandle, Focusable, FontWeight,
    InteractiveElement, IntoElement, KeyBinding, KeyDownEvent, ParentElement, Pixels, Render,
    SharedString, StatefulInteractiveElement, Styled, Window,
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
use prmarmot_core::board::{BoardScope, Mode};

use crate::state::{relative, AppState, SetupStatus};
use crate::table::{
    changed_marker_tooltip, columns_for, detail_text, label_chip, matches_filter,
    take_filter_chips, with_filter, BoardTableDelegate, FilterChip, Qualifier, StaleRule,
    TableWidthClass,
};
use crate::theme::ThemePref;
use crate::updates::{AutomaticCheck, CheckResult, InstallChannel, StableVersion};

// `FocusSearch` (⌘F / Ctrl-F, Edit → Find) is bound in `main`.
gpui::actions!(prmarmot, [CloseDetails, FocusSearch]);

const ALL_REPOS_LABEL: &str = "All repositories";
/// Chips (`label:`, `author:`, `repo:`, `is:`) the search box holds at most.
const MAX_FILTER_CHIPS: usize = 8;

fn scope_label(scope: &BoardScope) -> String {
    match scope {
        BoardScope::AllRepositories => ALL_REPOS_LABEL.to_owned(),
        BoardScope::Repository(repo) => repo.clone(),
    }
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
}

#[derive(Clone)]
struct AvailableUpdate {
    version: StableVersion,
    page_url: String,
    channel: InstallChannel,
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
    repo_status: String,
    search: Entity<InputState>,
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
    /// The current responsive width bucket, updated on window resize.
    width_class: TableWidthClass,
    /// Last-seen viewport width (px), the basis for the elastic Title/Note split.
    viewport_width: f32,
    /// Manually-resized column widths, remembered per (queue, width class) so a
    /// background refresh or a same-class window resize can't reset a layout the
    /// user dragged. Bounded by construction: 2 modes × 3 classes × 2 scopes
    /// = 12 entries.
    col_overrides: HashMap<(Mode, TableWidthClass, bool), Vec<Pixels>>,
    changed_only: bool,
    snoozed_expanded: bool,
    /// Loaded PRs matching the search that changed since you looked.
    changed_count: usize,
    /// Snoozed PRs among the rows the table shows.
    snoozed_count: usize,
    suppress_ack_for: Option<String>,
    pending_notification_pr: Option<String>,
    automatic_update_checks: bool,
    update_paths: crate::config::UpdatePaths,
    available_update: Option<AvailableUpdate>,
    update_error: Option<String>,
    update_check_pending: bool,
    update_starting: bool,
    update_check_task: Option<gpui::Task<()>>,
    notification_help_shown: bool,
}

impl RootView {
    pub fn new(
        state: Entity<AppState>,
        launch: Launch,
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
        let view = cx.entity().downgrade();
        let table = cx.new(|cx| {
            let mut delegate = BoardTableDelegate::new(mode, all_repos);
            let group_view = view.clone();
            let filter_view = view.clone();
            delegate.on_filter_click = Some(std::rc::Rc::new(move |chip, window, cx| {
                let _ = filter_view.update(cx, |this, cx| this.filter_by(chip, window, cx));
            }));
            delegate.on_row_action = Some(std::rc::Rc::new(move |row, action, window, cx| {
                let _ = view.update(cx, |this, cx| this.row_action(row, action, window, cx));
            }));
            delegate.on_group_copy = Some(std::rc::Rc::new(move |copy, _, cx| {
                let _ = group_view.update(cx, |this, cx| this.copy_group(copy, cx));
            }));
            delegate.set_columns(columns_for(mode, initial_class, initial_width, all_repos));
            TableState::new(delegate, window, cx)
                .sortable(false)
                .col_movable(false)
                .col_resizable(true)
                .row_selectable(true)
        });

        let repo_select = {
            let current = scope_label(&state.read(cx).scope);
            let mut picker_items = launch.repos.clone();
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
            cx.subscribe(
                &select,
                |this: &mut Self, _, event: &SelectEvent<SearchableVec<String>>, cx| {
                    let SelectEvent::Confirm(Some(repo)) = event else {
                        return;
                    };
                    this.select_scope(scope_from_label(repo), cx);
                },
            )
            .detach();
            // 0.6 renders the trigger from the filtered cursor, not the committed
            // value. Restore the full list on close (Escape or outside click).
            // Focusable exposes the popup handle while open, the trigger otherwise.
            let trigger = select.focus_handle(cx);
            let mut was_open = false;
            cx.observe_in(&select, window, move |_, select, window, cx| {
                let open = select.focus_handle(cx) != trigger;
                let closed = was_open && !open;
                was_open = open;
                if let Some(current) = select.read(cx).selected_value().cloned().filter(|_| closed)
                {
                    select.update(cx, |select, cx| {
                        select.set_selected_value(&current, window, cx);
                        cx.notify();
                    });
                }
            })
            .detach();
            select
        };

        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Filter loaded PRs…"));
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

        // Push new rows into the table only when a fetch actually landed —
        // gate the observer on generation (the PRFlow infinite-observer trap).
        // On a mode switch, restore that queue's remembered selection + scroll
        // once its rows are in place; on a plain refresh, keep the current
        // selection but clamp it if the row count shrank.
        cx.observe(&state, |this: &mut Self, state, cx| {
            let generation = state.read(cx).generation;
            if generation != this.seen_generation {
                this.seen_generation = generation;
                this.sync_table(cx);
            }
            cx.notify();
        })
        .detach();

        cx.subscribe(&table, |this, _table, event: &TableEvent, cx| match event {
            TableEvent::DoubleClickedRow(row_ix) => this.open_row(*row_ix, cx),
            TableEvent::SelectRow(row_ix) => this.on_select_row(*row_ix, cx),
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

        // Remember the window size across sessions (red traffic light path;
        // the `q` key saves too).
        window.on_window_should_close(cx, |window, _cx| {
            save_window_size(window);
            true
        });

        // The "synced Xm ago" label renders only on notify — without a slow
        // tick it can claim "just now" for a whole refresh interval. One
        // frame a minute; nothing animates.
        let ticker = cx.entity().downgrade();
        cx.spawn_in(window, async move |_this, cx| loop {
            cx.background_executor().timer(Duration::from_secs(5)).await;
            let Some(view) = ticker.upgrade() else { break };
            let _ = view.update_in(cx, |this, window, cx| {
                let selected = this.selected_row_id(cx);
                this.state.update(cx, |state, cx| {
                    state.set_focused_selection(selected, window.is_window_active());
                    state.wake_timed_snoozes(cx);
                });
                if let Some(event) = this.state.read(cx).take_platform_event() {
                    match event {
                        crate::platform::PlatformEvent::Clicked(pr_id) => {
                            window.activate_window();
                            this.select_notification_pr(pr_id, cx);
                        }
                        crate::platform::PlatformEvent::NotificationError(error) => {
                            this.state
                                .update(cx, |state, _| state.notification_error = Some(error));
                        }
                        crate::platform::PlatformEvent::NotificationPermissionChanged(
                            permission,
                        ) => {
                            #[cfg(target_os = "macos")]
                            if permission == crate::platform::NotificationPermission::Allowed {
                                this.state
                                    .update(cx, |state, _| state.notification_error = None);
                                this.show_feedback("Notifications are allowed", cx);
                                return;
                            }
                            this.state.update(cx, |state, _| {
                                state.notification_error =
                                    Some("Notifications need attention".into());
                            });
                            if !this.notification_help_shown {
                                this.notification_help_shown = true;
                                this.show_notification_help(permission, window, cx);
                            }
                        }
                        crate::platform::PlatformEvent::NotificationPermissionError {
                            operation,
                            message,
                        } => {
                            this.state.update(cx, |state, _| {
                                state.notification_error =
                                    Some(format!("{operation:?}: {message}"));
                            });
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();

        let mut this = Self {
            state,
            table,
            repo_select,
            configured_repos: launch.repos,
            pinned_repos: launch.pinned_repos,
            search_open: false,
            discovering_repos: false,
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
            width_class: initial_class,
            viewport_width: initial_width,
            col_overrides: HashMap::new(),
            changed_only: false,
            snoozed_expanded: false,
            changed_count: 0,
            snoozed_count: 0,
            suppress_ack_for: None,
            pending_notification_pr: None,
            automatic_update_checks: launch.automatic_update_checks,
            update_paths: launch.update_paths,
            available_update: None,
            update_error: launch.update_failure,
            update_check_pending: false,
            update_starting: false,
            update_check_task: None,
            notification_help_shown: false,
        };
        if this.state.read(cx).attention_preferences.notifications {
            this.state.read(cx).check_notification_permission();
        }
        this.state.update(cx, |state, cx| state.validate_setup(cx));
        this.start_refresh_loop(cx);
        this.discover_repos(window, cx);
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

    /// A label, author, or repository was clicked: filter by it, show the
    /// search box, and keep the keyboard on the board.
    fn filter_by(&mut self, chip: FilterChip, window: &mut Window, cx: &mut Context<Self>) {
        self.search_open = true;
        self.add_filter_chip(chip, cx);
        self.sync_table(cx);
        self.table.focus_handle(cx).focus(window, cx);
    }

    fn sync_table(&mut self, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let stale = StaleRule {
            now: Utc::now(),
            after_days: state.config.stale_after_days,
        };
        let matching: Vec<_> = state
            .rows
            .iter()
            .filter(|row| {
                matches_filter(row, &self.filter_text, stale)
                    && self
                        .filter_chips
                        .iter()
                        .all(|chip| chip.matches(row, stale))
            })
            .collect();
        self.changed_count = matching
            .iter()
            .filter(|row| state.is_changed(&row.id))
            .count();
        let rows: Vec<_> = matching
            .into_iter()
            .filter(|row| !self.changed_only || state.is_changed(&row.id))
            .cloned()
            .collect();
        let changed: HashMap<_, _> = rows
            .iter()
            .filter(|row| state.is_changed(&row.id))
            .map(|row| {
                let summary = state.change_summary(&row.id);
                (row.id.clone(), changed_marker_tooltip(&summary))
            })
            .collect();
        let watched: HashSet<_> = rows
            .iter()
            .filter(|row| state.is_watched(&row.id))
            .map(|row| row.id.clone())
            .collect();
        let snoozed: HashSet<_> = rows
            .iter()
            .filter(|row| state.snooze_description(&row.id).is_some())
            .map(|row| row.id.clone())
            .collect();
        self.visible_count = rows.len();
        self.snoozed_count = snoozed.len();
        let switching = self.pending_restore.take();
        let target_url = match switching {
            Some(mode) => self.selections.get(&mode).cloned(),
            None => self.selected_row_url(cx),
        };
        self.table.update(cx, |table, cx| {
            table.delegate_mut().set_stale_after_days(stale.after_days);
            table.delegate_mut().set_rows(rows);
            table
                .delegate_mut()
                .set_attention(changed, watched, snoozed, self.snoozed_expanded);
            table.refresh(cx);
            if let Some(ix) = target_url.and_then(|u| table.delegate().display_index_of_url(&u)) {
                self.suppress_ack_for = table.delegate().row(ix).map(|row| row.id.clone());
                table.set_selected_row(ix, cx);
                if switching.is_some() {
                    table.scroll_to_row(ix, cx);
                }
            } else {
                table.clear_selection(cx);
            }
        });
        if self.selected_row_url(cx).is_none() {
            self.details_open = false;
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
        self.details_open = false;
        let all_repos = scope.is_all();
        self.state.update(cx, |s, cx| s.switch_scope(scope, cx));
        let mode = self.state.read(cx).mode;
        let cols = self.columns_for_current(mode, cx);
        self.table.update(cx, |table, cx| {
            table.delegate_mut().set_scope(all_repos);
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
        let settings = cx.new(|cx| crate::settings::SettingsView::new(window, cx));
        cx.subscribe_in(
            &settings,
            window,
            |this, _, _: &crate::settings::SettingsSaved, window, cx| {
                let file = crate::config::load();
                let was_enabled = this.automatic_update_checks;
                this.automatic_update_checks = file.automatic_update_checks;
                this.theme_pref = ThemePref::resolve(file.theme.as_deref());
                this.theme_pref.apply(window, cx);
                let refresh = crate::state::refresh_interval(file.refresh_secs);
                if this.refresh != refresh {
                    this.refresh = refresh;
                    this.start_refresh_loop(cx);
                }
                this.state.update(cx, |state, cx| {
                    state.apply_attention_preferences(crate::state::AttentionPreferences {
                        notifications: file.notifications,
                        notification_sound: file.notification_sound,
                        notify_all_needs_action: file.notify_all_needs_action,
                        dock_badge: file.dock_badge,
                    });
                    state.apply_config(crate::board_config(&file), cx)
                });
                window.close_dialog(cx);
                this.table.focus_handle(cx).focus(window, cx);
                this.show_feedback("Settings saved", cx);
                if file.notifications {
                    this.notification_help_shown = false;
                    this.state.read(cx).check_notification_permission();
                }
                if !was_enabled && this.automatic_update_checks {
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

    fn discover_repos(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.discovering_repos {
            return;
        }
        self.discovering_repos = true;
        self.repo_status = "Finding accessible repositories…".into();
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async { prmarmot_core::github::gh_cli::list_repos() })
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
                        repos.insert(0, ALL_REPOS_LABEL.to_owned());
                        this.repo_status = format!(
                            "{} repositories{}",
                            repos.len(),
                            if discovery.truncated {
                                " · discovery limit reached"
                            } else {
                                ""
                            }
                        );
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
        self.refresh_task = Some(cx.spawn(async move |_this, cx| {
            loop {
                cx.background_executor().timer(interval).await;
                // Rate-limit gating and in-flight dedup live inside refresh().
                if state.update(cx, |s, cx| s.refresh(cx)).is_err() {
                    break; // app is shutting down
                }
            }
        }));
    }

    fn start_update_check_loop(&mut self, cx: &mut Context<Self>) {
        self.update_check_task = Some(cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(crate::updates::AUTOMATIC_CHECK_INTERVAL)
                .await;
            if this
                .update(cx, |this, cx| this.check_for_updates(cx))
                .is_err()
            {
                break;
            }
        }));
    }

    fn check_for_updates(&mut self, cx: &mut Context<Self>) {
        if !self.automatic_update_checks || self.update_check_pending {
            return;
        }
        self.update_check_pending = true;
        let state_path = self.update_paths.check_state.clone();
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move {
                    let identity = crate::updates::ReleaseIdentity::new(crate::RELEASE_REPO)?;
                    let source = crate::updates::GhReleaseSource::new(
                        prmarmot_core::github::gh_cli::resolve_gh_path(),
                    );
                    let checker = crate::updates::UpdateChecker::new(state_path, source);
                    let checked = checker.automatic_check(
                        true,
                        unix_now(),
                        env!("CARGO_PKG_VERSION"),
                        &identity,
                    )?;
                    let channel = if check_result(&checked)
                        .is_some_and(|result| matches!(result, CheckResult::Available { .. }))
                    {
                        #[cfg(target_os = "macos")]
                        {
                            Some(crate::updates::detect_current_install_channel(
                                crate::CASK_TOKEN,
                                &crate::updates::SystemCommandRunner,
                            )?)
                        }
                        #[cfg(not(target_os = "macos"))]
                        {
                            Some(InstallChannel::Direct)
                        }
                    } else {
                        None
                    };
                    Ok::<_, crate::updates::UpdateError>((checked, channel))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.update_check_pending = false;
                match outcome {
                    Ok((checked, channel)) => {
                        if let Some(CheckResult::Available {
                            version, page_url, ..
                        }) = check_result(&checked)
                        {
                            this.available_update = Some(AvailableUpdate {
                                version: *version,
                                page_url: page_url.clone(),
                                channel: channel.unwrap_or(InstallChannel::Direct),
                            });
                        } else if matches!(
                            check_result(&checked),
                            Some(CheckResult::UpToDate { .. })
                        ) {
                            this.available_update = None;
                        }
                    }
                    Err(error) => eprintln!("prmarmot: automatic update check failed: {error}"),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn begin_update(&mut self, window: &Window, cx: &mut Context<Self>) {
        let Some(available) = self.available_update.clone() else {
            return;
        };
        match available.channel {
            InstallChannel::Direct => cx.open_url(&available.page_url),
            InstallChannel::Homebrew { brew_path, .. } => {
                if self.update_starting {
                    return;
                }
                self.update_starting = true;
                self.update_error = None;
                let invocation = match crate::updates::UpgradeIdentity::new(
                    crate::CASK_TOKEN,
                    crate::APPLICATION_NAME,
                ) {
                    Ok(identity) => crate::updates::HelperInvocation {
                        parent_pid: std::process::id(),
                        brew_path,
                        identity,
                        receipt_path: self.update_paths.upgrade_receipt.clone(),
                        lock_path: self.update_paths.helper_lock.clone(),
                        open_path: PathBuf::from("/usr/bin/open"),
                    },
                    Err(error) => {
                        self.update_starting = false;
                        self.update_error = Some(error.to_string());
                        cx.notify();
                        return;
                    }
                };
                let executable = match std::env::current_exe() {
                    Ok(executable) => executable,
                    Err(error) => {
                        self.update_starting = false;
                        self.update_error = Some(format!("Could not start update: {error}"));
                        cx.notify();
                        return;
                    }
                };
                let window_size = window.window_bounds();
                cx.spawn(async move |this, cx| {
                    let result = cx
                        .background_executor()
                        .spawn(async move {
                            crate::updates::spawn_upgrade_helper(&executable, &invocation)
                        })
                        .await;
                    let _ = this.update(cx, |this, cx| match result {
                        Ok(_) => {
                            let (gpui::WindowBounds::Windowed(bounds)
                            | gpui::WindowBounds::Maximized(bounds)
                            | gpui::WindowBounds::Fullscreen(bounds)) = window_size;
                            crate::config::persist_window(
                                bounds.size.width.into(),
                                bounds.size.height.into(),
                            );
                            // The detached copy is alive and waiting for this
                            // PID. Quit only after that spawn succeeds.
                            cx.quit();
                        }
                        Err(error) => {
                            this.update_starting = false;
                            this.update_error = Some(format!("Could not start update: {error}"));
                            cx.notify();
                        }
                    });
                })
                .detach();
            }
        }
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
            },
        );
    }

    /// The columns for `mode` at the current width class: a stored manual
    /// override if one is present (and still matches the column count), else the
    /// responsive default for this window width.
    fn columns_for_current(&self, mode: Mode, cx: &App) -> Vec<Column> {
        let all_repos = self.state.read(cx).scope.is_all();
        let mut cols = columns_for(mode, self.width_class, self.viewport_width, all_repos);
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
        let width: f32 = window.viewport_size().width.into();
        let new_class = TableWidthClass::from_width(width);
        let class_changed = new_class != self.width_class;
        self.width_class = new_class;
        self.viewport_width = width;
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

    /// Keep keyboard/mouse selection off the section-header pseudo-rows: when a
    /// header gets selected, bounce to the nearest PR in the direction of
    /// travel (`set_selected_row` re-emits `SelectRow`, but the bounced-to row
    /// is a real PR, so it settles in one hop).
    fn on_select_row(&mut self, row_ix: usize, cx: &mut Context<Self>) {
        let delegate_is_header =
            |i: usize, this: &Self, cx: &Context<Self>| this.table.read(cx).delegate().is_header(i);
        if !delegate_is_header(row_ix, self, cx) {
            self.last_selected = row_ix;
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
            }
            cx.notify();
            return;
        }
        let len = self.table.read(cx).delegate().display_len();
        let going_down = row_ix >= self.last_selected;
        let down = (row_ix + 1..len).find(|&i| !delegate_is_header(i, self, cx));
        let up = (0..row_ix)
            .rev()
            .find(|&i| !delegate_is_header(i, self, cx));
        let target = if going_down { down.or(up) } else { up.or(down) };
        if let Some(t) = target {
            self.last_selected = t;
            self.table
                .update(cx, |table, cx| table.set_selected_row(t, cx));
        }
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
            self.details_open = false;
            self.table.focus_handle(cx).focus(window, cx);
            cx.notify();
            return;
        }
        // Text inputs own character keys; toolbar buttons do not. The select's
        // dynamic focus handle includes its searchable popup.
        if self.search.focus_handle(cx).contains_focused(window, cx)
            || self
                .repo_select
                .focus_handle(cx)
                .contains_focused(window, cx)
            || event.keystroke.modifiers.platform
            || event.keystroke.modifiers.control
            || event.keystroke.modifiers.alt
        {
            return;
        }
        let table_focused = self.table.focus_handle(cx).contains_focused(window, cx);
        let key = event.keystroke.key.as_str();
        let platform = event.keystroke.modifiers.platform;
        match key {
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
                let mode = match self.state.read(cx).mode {
                    Mode::Authored => Mode::Review,
                    Mode::Review => Mode::Authored,
                };
                self.select_mode(mode, cx);
            }
            "1" if !platform => self.select_mode(Mode::Authored, cx),
            "2" if !platform => self.select_mode(Mode::Review, cx),
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

    /// `Y`: copy the group containing the selected PR as a list.
    fn copy_selected_group(&mut self, cx: &mut Context<Self>) {
        let copy = {
            let table = self.table.read(cx);
            table.selected_row().and_then(|row_ix| {
                let delegate = table.delegate();
                let label = delegate.group_label_at(row_ix)?;
                delegate.group_copy(&label, prmarmot_core::share::ShareFormat::List)
            })
        };
        if let Some(copy) = copy {
            self.copy_group(copy, cx);
        }
    }

    fn row_action(
        &mut self,
        row: prmarmot_core::board::BoardRow,
        action: crate::table::RowAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::table::RowAction;
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
                    self.details_open = true;
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
            self.show_feedback_owned(message, cx);
            self.sync_table(cx);
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
        window.open_dialog(cx, move |dialog, _, cx| {
            let _ = cx;
            let mut options = v_flex().gap_2();
            let choices = [
                (
                    "One hour",
                    crate::attention_state::SnoozeCondition::Until {
                        deadline: Utc::now() + ChronoDuration::hours(1),
                    },
                ),
                (
                    "Until tomorrow",
                    crate::attention_state::SnoozeCondition::Until {
                        deadline: Utc::now() + ChronoDuration::hours(24),
                    },
                ),
                (
                    "Waiting for CI",
                    crate::attention_state::AttentionState::waiting_ci(&row),
                ),
                (
                    "Review again when changed",
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
            if let Some(author) = row.author.clone() {
                let state = state.clone();
                let waiting =
                    crate::attention_state::AttentionState::waiting_person(&row, author.clone());
                let row = row.clone();
                options = options.child(
                    Button::new("snooze-person")
                        .label(format!("Waiting on {author}"))
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
                        .label("Cancel snooze")
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

    fn select_notification_pr(&mut self, pr_id: String, cx: &mut Context<Self>) {
        let current = self
            .state
            .read(cx)
            .rows
            .iter()
            .find(|row| row.id == pr_id)
            .cloned();
        if let Some(row) = current {
            self.sync_table(cx);
            if let Some(index) = self
                .table
                .read(cx)
                .delegate()
                .display_index_of_url(&row.url)
            {
                self.table.update(cx, |table, cx| {
                    table.set_selected_row(index, cx);
                    table.scroll_to_row(index, cx);
                });
            }
        } else {
            if let Some((url, status)) = self.state.read(cx).watched_fallback(&pr_id) {
                cx.open_url(&url);
                self.show_feedback_owned(format!("Watched PR opened · {status}"), cx);
            } else {
                self.pending_notification_pr = Some(pr_id);
            }
        }
    }

    fn toggle_details(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.details_open {
            self.details_open = false;
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
            self.details_open = self.selected_row_url(cx).is_some();
        }
        self.table.focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    fn render_tools(&self, cx: &Context<Self>) -> impl IntoElement {
        let state = self.state.read(cx);
        let current_repo = state.scope.repository();
        let pinned = self
            .pinned_repos
            .iter()
            .any(|pin| current_repo.is_some_and(|repo| pin.eq_ignore_ascii_case(repo)));
        h_flex()
            .px(px(16.))
            .py(px(6.))
            .gap_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .when(!self.search_open, |bar| {
                bar.child(
                    h_flex()
                        .id("pinned-repos")
                        .flex_1()
                        .min_w_0()
                        .overflow_x_scroll()
                        .gap_2()
                        .when(self.pinned_repos.is_empty(), |pins| {
                            pins.child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(cx.theme().muted_foreground)
                                    .child("Pin your frequent repositories"),
                            )
                        })
                        .children(self.pinned_repos.iter().enumerate().map(|(ix, repo)| {
                            let target = repo.clone();
                            Button::new(("pinned-repo", ix))
                                .small()
                                .child(div().max_w(px(160.)).truncate().child(repo.clone()))
                                .when(
                                    current_repo
                                        .is_some_and(|current| repo.eq_ignore_ascii_case(current)),
                                    |button| {
                                        button
                                            .bg(cx.theme().accent)
                                            .text_color(cx.theme().accent_foreground)
                                    },
                                )
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.select_scope(BoardScope::Repository(target.clone()), cx);
                                    this.repo_select.update(cx, |select, cx| {
                                        select.set_selected_value(&target, window, cx);
                                    });
                                    this.table.focus_handle(cx).focus(window, cx);
                                }))
                        })),
                )
                .child(
                    Button::new("pin-current")
                        .small()
                        .w(px(60.))
                        .label(if pinned { "Pinned" } else { "Pin" })
                        .when(pinned, |button| button.bg(cx.theme().secondary))
                        .tooltip(if pinned {
                            "Unpin this repository"
                        } else if self.pinned_repos.len() >= crate::config::MAX_PINNED_REPOS {
                            "Unpin a repository first (12 pins maximum)"
                        } else {
                            "Pin this repository"
                        })
                        .disabled(
                            current_repo.is_none()
                                || (!pinned
                                    && self.pinned_repos.len() >= crate::config::MAX_PINNED_REPOS),
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_pin(cx))),
                )
                .child(
                    Button::new("open-search")
                        .small()
                        .label("Search · /")
                        .tooltip(if cfg!(target_os = "macos") {
                            "Filter loaded PRs (/ or ⌘F). Type label:, author:, repo:, or is:stale, or click a label, author, or repository."
                        } else {
                            "Filter loaded PRs (/ or Ctrl F). Type label:, author:, repo:, or is:stale, or click a label, author, or repository."
                        })
                        .on_click(cx.listener(|this, _, window, cx| this.open_search(window, cx))),
                )
            })
            .when(self.search_open, |bar| {
                bar.child(self.render_search_box(cx))
            })
            .when(self.filtering() || self.changed_only, |bar| {
                bar.child(
                    div()
                        .text_size(px(12.))
                        .text_color(cx.theme().muted_foreground)
                        .child(format!(
                            "{} of {} loaded",
                            self.visible_count,
                            state.rows.len()
                        )),
                )
            })
            .child(
                div()
                    .w(px(1.))
                    .h(px(16.))
                    .flex_shrink_0()
                    .bg(cx.theme().border),
            )
            .child(
                view_toggle(
                    "changed-filter",
                    "Changed",
                    self.changed_count,
                    self.changed_only,
                    Some(cx.theme().link),
                    cx,
                )
                .tooltip(changed_toggle_tooltip(
                    self.changed_only,
                    self.changed_count,
                ))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.changed_only = !this.changed_only;
                    this.sync_table(cx);
                })),
            )
            .child(
                view_toggle(
                    "snoozed-toggle",
                    "Snoozed",
                    self.snoozed_count,
                    self.snoozed_expanded,
                    None,
                    cx,
                )
                .tooltip(snoozed_toggle_tooltip(
                    self.snoozed_expanded,
                    self.snoozed_count,
                ))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.snoozed_expanded = !this.snoozed_expanded;
                    this.sync_table(cx);
                })),
            )
            .when(self.search_open, |bar| bar.child(div().flex_1()))
            .when(state.truncated, |bar| {
                bar.child(
                    Button::new("load-more")
                        .small()
                        .label(if state.syncing {
                            "Loading…"
                        } else if state.backoff_remaining().is_some() {
                            "Paused"
                        } else if state.can_load_more() {
                            "Load more"
                        } else if state.page_limit_reached() {
                            "Page limit reached"
                        } else {
                            "Refresh to retry"
                        })
                        .disabled(!state.can_load_more())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.state.update(cx, |state, cx| state.load_more(cx))
                        })),
                )
            })
            .child(
                Button::new("show-details")
                    .small()
                    .label(if self.details_open {
                        "Hide details"
                    } else {
                        "Details · Space"
                    })
                    .disabled(self.visible_count == 0)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.toggle_details(window, cx);
                    })),
            )
    }

    fn render_details(&self, cx: &Context<Self>) -> impl IntoElement {
        let table = self.table.read(cx);
        let selected = table
            .selected_row()
            .and_then(|ix| table.delegate().row(ix))
            .cloned();
        let theme = cx.theme();
        let (hover_border, hover_text) = (theme.muted_foreground, theme.foreground);
        v_flex()
            .h(px(210.))
            .flex_shrink_0()
            .border_t_1()
            .border_color(theme.border)
            .bg(theme.background)
            .px(px(16.))
            .py(px(10.))
            .gap_2()
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("PR details"),
                    )
                    .when_some(selected.clone(), |bar, row| {
                        let copy_row = row.clone();
                        let view = cx.entity().downgrade();
                        bar.child(
                            Button::new("copy-detail")
                                .small()
                                .label("Copy")
                                .dropdown_caret(true)
                                .dropdown_menu(move |mut menu, _, _| {
                                    for (label, text) in crate::table::row_copy_items(&copy_row) {
                                        let view = view.clone();
                                        menu = menu.item(PopupMenuItem::new(label).on_click(
                                            move |_, _, cx| {
                                                cx.write_to_clipboard(ClipboardItem::new_string(
                                                    text.clone(),
                                                ));
                                                let _ = view.update(cx, |this, cx| {
                                                    this.show_feedback("Copied to clipboard", cx)
                                                });
                                            },
                                        ));
                                    }
                                    menu
                                }),
                        )
                    })
                    .when_some(selected.clone(), |bar, row| {
                        bar.child(
                            Button::new("open-detail")
                                .small()
                                .label("Open on GitHub")
                                .on_click(move |_, _, cx| cx.open_url(&row.url)),
                        )
                    })
                    .child(
                        Button::new("close-details")
                            .small()
                            .label("Close · Esc")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.details_open = false;
                                this.table.focus_handle(cx).focus(window, cx);
                                cx.notify();
                            })),
                    ),
            )
            .child(
                v_flex()
                    .id("pr-details-content")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .gap_2()
                    .map(|content| match selected {
                        Some(row) => content
                            .child(div().font_weight(FontWeight::SEMIBOLD).child(
                                SelectableText::new(
                                    "detail-title",
                                    format!("#{}  {}", row.number, row.title),
                                ),
                            ))
                            // Labels as chips: a click filters by the label, as in the table.
                            .when(!row.labels.is_empty(), |content| {
                                content.child(
                                    h_flex()
                                        .flex_wrap()
                                        .gap_1()
                                        .text_size(px(13.))
                                        .child("Labels:")
                                        .children(row.labels.iter().enumerate().map(
                                            |(ix, label)| {
                                                let target =
                                                    FilterChip::new(Qualifier::Label, label);
                                                let tip = format!("Filter by {}", target.term());
                                                label_chip(theme)
                                                    .id(("detail-label", ix))
                                                    .cursor_pointer()
                                                    .text_color(theme.secondary_foreground)
                                                    .hover(|style| {
                                                        style
                                                            .border_color(hover_border)
                                                            .text_color(hover_text)
                                                    })
                                                    .child(label.clone())
                                                    .tooltip(move |window, cx| {
                                                        Tooltip::new(tip.clone()).build(window, cx)
                                                    })
                                                    .on_click(cx.listener(
                                                        move |this, _, window, cx| {
                                                            this.filter_by(
                                                                target.clone(),
                                                                window,
                                                                cx,
                                                            )
                                                        },
                                                    ))
                                            },
                                        )),
                                )
                            })
                            .child(div().text_size(px(13.)).child(SelectableText::new(
                                "detail-body",
                                {
                                    // The labels line is the chip row above.
                                    let mut text = detail_text(&row).replace(
                                        &format!("\nLabels: {}", row.labels.join(", ")),
                                        "",
                                    );
                                    let state = self.state.read(cx);
                                    text.push_str(&format!(
                                        "\nAttention: {} · {}",
                                        if state.is_changed(&row.id) {
                                            "changed"
                                        } else {
                                            "acknowledged"
                                        },
                                        if state.is_watched(&row.id) {
                                            "watched"
                                        } else {
                                            "not watched"
                                        }
                                    ));
                                    if let Some(snooze) = state.snooze_description(&row.id) {
                                        text.push_str(&format!("\n{snooze}"));
                                    }
                                    text
                                },
                            ))),
                        None => content.child("Select a PR to inspect its details."),
                    }),
            )
    }

    fn render_header(&self, cx: &Context<Self>) -> impl IntoElement {
        let state = self.state.read(cx);
        let theme = cx.theme();
        // Just the total: the section headers already carry the per-category
        // breakdown, so repeating "N need action · N awaiting…" here is
        // redundant and truncates at narrow widths (design review). This frees
        // titlebar room. "Loaded", not "shown": search, Changed, and a
        // collapsed Snoozed group hide rows; the tools bar says how many.
        let (counts, counts_tip) = header_counts(&HeaderCounts {
            loaded: state.rows.len(),
            truncated: state.truncated,
            need_you: state.badge_count,
            need_you_complete: state.badge_coverage_complete,
            tracked_loaded: state.tracked_loaded,
            tracked_total: state.tracked_total,
        });
        // Status priority: a hard error wins; then a rate-limit back-off (so a
        // switch into a paused window shows "paused", not a permanent
        // "Loading…"); otherwise the queue-specific sync line. Static text only
        // — a spinner would defeat the idle-GPU half of the spike gate.
        let (status_text, status_color) = if state.setup == SetupStatus::Checking {
            ("Checking GitHub CLI…".to_owned(), theme.muted_foreground)
        } else if state.setup != SetupStatus::Ready {
            ("GitHub setup required".to_owned(), theme.warning)
        } else if let Some(err) = state.error.clone() {
            (err, theme.danger)
        } else if let Some(secs) = state.backoff_remaining() {
            (
                format!("paused · retry in {}", human_duration(secs)),
                theme.warning,
            )
        } else {
            (
                queue_sync_text(
                    state.mode,
                    state.scope.is_all(),
                    state.syncing,
                    state.last_synced,
                ),
                theme.muted_foreground,
            )
        };
        let budget = state
            .rate
            .as_ref()
            .map(|r| format!("API {}/{}", r.remaining, r.limit));
        let selected_mode = match state.mode {
            Mode::Authored => 0,
            Mode::Review => 1,
        };
        // The queue is a primary scope, not a hidden preference. A compact
        // toolbar tab view keeps both choices visible and the selected state
        // persistent (Apple HIG); equal widths prevent either queue from
        // appearing subordinate. Keyboard 1/2 and v remain accelerators.
        let view_switcher = TabBar::new("view-switcher")
            .small()
            .segmented()
            .selected_index(selected_mode)
            .child(
                Tab::new()
                    .label(if state.scope.is_all() {
                        "Involving me"
                    } else {
                        "My PRs"
                    })
                    .w(px(104.))
                    .font_weight(if state.mode == Mode::Authored {
                        FontWeight::SEMIBOLD
                    } else {
                        FontWeight::MEDIUM
                    }),
            )
            .child(Tab::new().label("Review queue").w(px(104.)).font_weight(
                if state.mode == Mode::Review {
                    FontWeight::SEMIBOLD
                } else {
                    FontWeight::MEDIUM
                },
            ))
            .on_click(cx.listener(|this, index: &usize, _, cx| {
                let mode = match index {
                    0 => Mode::Authored,
                    _ => Mode::Review,
                };
                this.select_mode(mode, cx);
            }));

        // One-line toolbar living INSIDE the transparent titlebar: repository
        // identity, primary queue scope, flexible counts, then sync status.
        // An error replaces the sync text — it IS the sync status then.
        h_flex()
            .flex_1()
            .min_w_0()
            // TitleBar's inner container does not shrink to its viewport in 0.6.
            // Reserve its platform chrome and bound our flexible content explicitly.
            .max_w(px(
                self.viewport_width - if cfg!(target_os = "macos") { 80. } else { 114. }
            ))
            .pr(px(crate::design::HEADER_PAD_X))
            .gap_3()
            .items_center()
            .child(
                div()
                    .w(px(220.))
                    .flex_shrink_0()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(
                        Select::new(&self.repo_select)
                            .small()
                            .menu_width(px(420.))
                            .search_placeholder("Search accessible repositories…")
                            .accessibility_label("Repository"),
                    ),
            )
            .child(view_switcher)
            .child(
                div()
                    .id("header-counts")
                    .min_w_0()
                    .flex_1()
                    .truncate()
                    .text_size(px(13.))
                    .text_color(theme.muted_foreground)
                    .child(counts)
                    .tooltip(move |window, cx| Tooltip::new(counts_tip.clone()).build(window, cx)),
            )
            .child(
                h_flex()
                    .flex_shrink_0()
                    .max_w(px(self.viewport_width * 0.28))
                    .gap_3()
                    .text_size(px(12.))
                    .text_color(theme.muted_foreground)
                    .child(
                        div()
                            .id("sync-status")
                            .min_w_0()
                            .truncate()
                            .text_color(status_color)
                            .child(status_text.clone())
                            .tooltip(move |window, cx| {
                                Tooltip::new(status_text.clone()).build(window, cx)
                            }),
                    )
                    .when_some(budget, |this, b| this.child(div().flex_shrink_0().child(b))),
            )
    }

    fn render_update_banners(&self, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        v_flex()
            .flex_shrink_0()
            .when_some(self.update_error.clone(), |banners, error| {
                let tooltip = error.clone();
                banners.child(
                    h_flex()
                        .px(px(crate::design::HEADER_PAD_X))
                        .py_1()
                        .gap_2()
                        .bg(theme.muted)
                        .border_b_1()
                        .border_color(theme.border)
                        .child(
                            div()
                                .id("update-error-message")
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_size(px(12.))
                                .text_color(theme.danger)
                                .child(format!("Update failed — {error}"))
                                .tooltip(move |window, cx| {
                                    Tooltip::new(tooltip.clone()).build(window, cx)
                                }),
                        )
                        .child(
                            Button::new("dismiss-update-error")
                                .small()
                                .ghost()
                                .label("Dismiss")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.update_error = None;
                                    cx.notify();
                                })),
                        ),
                )
            })
            .when_some(self.available_update.clone(), |banners, update| {
                banners.child(
                    h_flex()
                        .px(px(crate::design::HEADER_PAD_X))
                        .py_1()
                        .gap_2()
                        .bg(theme.secondary)
                        .border_b_1()
                        .border_color(theme.border)
                        .child(
                            div()
                                .flex_1()
                                .text_size(px(12.))
                                .font_weight(FontWeight::MEDIUM)
                                .child(format!("v{} available", update.version)),
                        )
                        .child(
                            Button::new("install-update")
                                .small()
                                .primary()
                                .label(if self.update_starting {
                                    "Starting update…"
                                } else {
                                    "Update"
                                })
                                .disabled(self.update_starting)
                                .on_click(
                                    cx.listener(|this, _, window, cx| {
                                        this.begin_update(window, cx)
                                    }),
                                ),
                        ),
                )
            })
    }

    fn render_footer(&self, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let hints: Vec<(&str, String)> = vec![
            ("↑↓", "select".into()),
            ("⏎", "open".into()),
            ("w", "watch".into()),
            ("s", "snooze".into()),
            ("r", "refresh".into()),
        ];
        // Keycap legend (spec §6): reference material lives at the bottom,
        // status at the top — the gh-dash/native pattern.
        let mut bar = h_flex()
            .flex_shrink_0()
            .px(px(crate::design::HEADER_PAD_X))
            .py(px(crate::design::FOOTER_PAD_Y))
            .gap_3()
            .items_center()
            .bg(theme.title_bar)
            .border_t_1()
            .border_color(theme.title_bar_border);
        for (key, label) in hints {
            bar = bar.child(
                h_flex()
                    .gap_1()
                    .items_center()
                    .child(
                        div()
                            .px_1()
                            .rounded(px(3.))
                            .bg(theme.muted)
                            .border_1()
                            .border_color(theme.border)
                            .text_size(px(11.))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.secondary_foreground)
                            .child(key),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(theme.muted_foreground)
                            .child(label),
                    ),
            );
        }
        bar.child(
            Button::new("shortcuts")
                .small()
                .ghost()
                .label("Shortcuts")
                .on_click(cx.listener(|this, _, window, cx| this.show_shortcuts(window, cx))),
        )
        .child(div().flex_1())
        .when_some(self.feedback.clone(), |bar, message| {
            bar.child(
                div()
                    .text_size(px(12.))
                    .text_color(theme.foreground)
                    .child(message),
            )
        })
        .when_some(
            self.state
                .read(cx)
                .attention
                .as_ref()
                .and_then(|attention| attention.storage_error.clone()),
            |bar, error| {
                bar.child(
                    div()
                        .id("attention-storage-error")
                        .max_w(px(260.))
                        .truncate()
                        .text_size(px(11.))
                        .text_color(theme.warning)
                        .child(error.clone())
                        .tooltip(move |window, cx| Tooltip::new(error.clone()).build(window, cx)),
                )
            },
        )
        .when_some(
            self.state.read(cx).notification_error.clone(),
            |bar, error| {
                bar.child(
                    Button::new("notification-help")
                        .small()
                        .label("Enable notifications…")
                        .tooltip(error)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.notification_help_shown = false;
                            this.state.read(cx).check_notification_permission();
                        })),
                )
            },
        )
        .child(
            div()
                .min_w_0()
                .truncate()
                .text_size(px(11.))
                .text_color(theme.muted_foreground)
                .child(self.repo_status.clone()),
        )
        .child(
            Button::new("discover-repos")
                .small()
                .label("Repos")
                .on_click(cx.listener(|this, _, window, cx| this.discover_repos(window, cx))),
        )
        .child(
            Button::new("configuration")
                .small()
                .label("Settings")
                .tooltip("Reviewer suggestions, refresh interval and appearance")
                .on_click(cx.listener(|this, _, window, cx| this.show_config(window, cx))),
        )
    }

    /// One search box: a search glyph, the label tokens (each removable),
    /// the typed words, and one × that clears everything and closes it.
    fn render_search_box(&self, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (muted, hover_bg, hover_text) =
            (theme.muted_foreground, theme.secondary, theme.foreground);
        let icon = |path: &'static str, size: f32| {
            gpui::svg()
                .path(path)
                .size(px(size))
                .flex_shrink_0()
                .text_color(muted)
        };
        let tokens = h_flex()
            .id("filter-chips")
            .max_w(px(360.))
            .overflow_x_scroll()
            .gap_1()
            .children(self.filter_chips.iter().enumerate().map(|(ix, chip)| {
                let tip = format!("Stop filtering by {}", chip.term());
                // "label:" as typed, so the chip teaches the syntax.
                label_chip(theme)
                    .flex_shrink_0()
                    .gap(px(3.))
                    .pr(px(2.))
                    .text_color(theme.secondary_foreground)
                    .child(
                        h_flex()
                            .child(
                                div()
                                    .text_color(muted)
                                    .child(format!("{}:", chip.qualifier.key())),
                            )
                            .child(div().max_w(px(140.)).truncate().child(chip.value.clone())),
                    )
                    .child(
                        div()
                            .id(("remove-filter-chip", ix))
                            .size(px(14.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(3.))
                            .cursor_pointer()
                            .hover(|style| style.bg(hover_bg))
                            .child(icon("icons/close.svg", 9.))
                            .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.remove_filter_chip(ix, window, cx)
                            })),
                    )
            }));
        let close = div()
            .id("close-search")
            .size(px(18.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(4.))
            .cursor_pointer()
            .hover(|style| style.bg(hover_bg).text_color(hover_text))
            .child(icon("icons/close.svg", 11.))
            .tooltip(|window, cx| Tooltip::new("Clear and close search").build(window, cx))
            .on_click(cx.listener(|this, _, window, cx| this.close_search(window, cx)));
        // Nothing to clear in an empty box; leaving it closes it.
        let has_content = self.filtering() || !self.search.read(cx).value().is_empty();
        // Wider with tokens, so the words keep room to type.
        let width = 300. + 110. * self.filter_chips.len().min(3) as f32;
        div().w(px(width)).flex_shrink_0().child(
            Input::new(&self.search)
                .small()
                .aria_label("Filter loaded PRs")
                .prefix(
                    h_flex()
                        .gap_1p5()
                        .child(icon("icons/search.svg", 13.))
                        .when(!self.filter_chips.is_empty(), |prefix| prefix.child(tokens)),
                )
                .when(has_content, |input| input.suffix(close)),
        )
    }

    fn show_shortcuts(&self, window: &mut Window, cx: &mut Context<Self>) {
        let focus = self.table.focus_handle(cx);
        window.open_dialog(cx, move |dialog, window, cx| {
            let mut rows = v_flex().id("shortcut-list").overflow_y_scroll()
                .max_h((window.viewport_size().height - px(300.)).min(px(450.)))
                .gap_2().text_size(px(13.));
            for (label, keys) in [
                ("Select a PR", "↑ / ↓"),
                ("Open selected PR", "Enter / o"),
                ("Copy selected PR URL", "y"),
                ("Copy selected PR's group as a list", "Y"),
                ("Watch / unwatch selected PR", "w"),
                ("Snooze selected PR", "s"),
                ("My PRs / Review queue", "1 / 2"),
                ("Switch queue", "v"),
                ("Refresh", "r"),
                (
                    "Search loaded PRs",
                    if cfg!(target_os = "macos") { "/ or ⌘F" } else { "/ or Ctrl F" },
                ),
                ("Search one label, author, or repo", "label: author: repo:"),
                ("Search PRs waiting too long for a reviewer", "is:stale"),
                ("Remove the last search filter", "⌫ in empty search"),
                ("Toggle selected PR details", "Space"),
                ("Cycle theme", "t"),
                ("Close dialog or details", "Esc"),
                ("Quit", if cfg!(target_os = "macos") { "⌘Q / q" } else { "Ctrl Q / q" }),
            ] {
                rows = rows.child(h_flex().justify_between().child(label).child(
                    div().px_1().rounded(px(3.)).bg(cx.theme().muted)
                        .text_color(cx.theme().muted_foreground).child(keys),
                ));
            }
            let focus = focus.clone();
            dialog.title("Keyboard shortcuts").w(px(420.)).close_button(false).child(rows)
                .child(div().mt_3().text_size(px(12.)).text_color(cx.theme().muted_foreground)
                    .child("Clicking a label, author, or repository in the table adds it to the search. Quote values with spaces: label:\"help wanted\"."))
                .child(div().mt_2().text_size(px(12.)).text_color(cx.theme().muted_foreground)
                    .child("Typing in search, the repository picker, or Settings never triggers dashboard shortcuts. Arrow keys, Enter, and Space act on the focused control."))
                .child(h_flex().mt_3().justify_end().child(Button::new("close-shortcuts")
                    .label("Done").on_click(|_, window, cx| window.close_dialog(cx))))
                .on_close(move |_, window, cx| focus.focus(window, cx))
        });
    }
}

/// A toolbar toggle with a fixed label, the count it applies to, and an
/// optional dot. When on it takes the accent, like the current pinned repo.
fn view_toggle(
    id: &'static str,
    label: &'static str,
    count: usize,
    on: bool,
    dot: Option<gpui::Hsla>,
    cx: &App,
) -> Button {
    let theme = cx.theme();
    let count_color = if on {
        theme.accent_foreground
    } else {
        theme.muted_foreground
    };
    // The content is a row, not a label, so name it for screen readers.
    let name: SharedString = if count > 0 {
        format!("{label} ({count})").into()
    } else {
        label.into()
    };
    Button::new(id)
        .small()
        .toggled(on)
        .accessibility_label(name)
        .child(
            h_flex()
                .gap_1p5()
                .items_center()
                .when_some(dot.filter(|_| count > 0), |row, color| {
                    row.child(
                        div()
                            .size(px(crate::design::STATUS_DOT))
                            .rounded_full()
                            .bg(color),
                    )
                })
                .child(label)
                .when(count > 0, |row| {
                    row.child(
                        div()
                            .text_size(px(11.))
                            .text_color(count_color)
                            .child(count.to_string()),
                    )
                }),
        )
        .when(on, |button| {
            button.bg(theme.accent).text_color(theme.accent_foreground)
        })
}

fn changed_toggle_tooltip(on: bool, count: usize) -> String {
    match (on, count) {
        (true, _) => "Showing only PRs that changed since you looked. Click to show all.".into(),
        (false, 0) => "No loaded PRs changed since you looked".into(),
        (false, 1) => "Show only the PR that changed since you looked".into(),
        (false, n) => format!("Show only the {n} PRs that changed since you looked"),
    }
}

fn snoozed_toggle_tooltip(on: bool, count: usize) -> String {
    match (on, count) {
        (true, _) => "Collapse the snoozed PRs".into(),
        (false, 0) => "No snoozed PRs here".into(),
        (false, 1) => "Show the snoozed PR".into(),
        (false, n) => format!("Show the {n} snoozed PRs"),
    }
}

/// The numbers behind the header's count line.
struct HeaderCounts {
    loaded: usize,
    truncated: bool,
    /// The Dock badge: your PRs that need action plus reviews requested from
    /// you, snoozed ones excluded.
    need_you: usize,
    /// Both queues have loaded, so `need_you` is the whole count.
    need_you_complete: bool,
    tracked_loaded: usize,
    tracked_total: usize,
}

/// The header's count line and the tooltip that explains it.
fn header_counts(c: &HeaderCounts) -> (String, String) {
    let so_far = if c.need_you_complete { "" } else { " so far" };
    let need_you = match c.need_you {
        0 => format!("nothing needs you{so_far}"),
        1 => format!("1 needs you{so_far}"),
        n => format!("{n} need you{so_far}"),
    };
    let mut line = format!("{} loaded", c.loaded);
    if c.truncated {
        line.push_str(" · partial results");
    }
    line.push_str(&format!(" · {need_you}"));
    let tracked = match (c.tracked_loaded, c.tracked_total) {
        (_, 0) => None,
        (loaded, total) if loaded >= total => Some(format!("{total} watched/snoozed")),
        (loaded, total) => Some(format!("{loaded} of {total} watched/snoozed")),
    };
    if let Some(tracked) = &tracked {
        line.push_str(&format!(" · {tracked}"));
    }

    let mut tip = format!("{} PRs loaded in this view", c.loaded);
    tip.push_str(if c.truncated {
        "; GitHub has more (Load more)."
    } else {
        "."
    });
    tip.push_str(&format!(
        "\n{}: your PRs that need action plus reviews requested from you, not counting \
         snoozed ones. The Dock badge shows the same number.",
        upper_first(&need_you)
    ));
    if !c.need_you_complete {
        tip.push_str(" Only the queues loaded since launch are counted.");
    }
    if let Some(tracked) = tracked {
        tip.push_str(&format!(
            "\n{}: watched and snoozed PRs, refreshed with this view (up to 50 each time).",
            upper_first(&tracked)
        ));
    }
    (line, tip)
}

fn upper_first(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// The header's right-side status line, specific to the active queue. Keeps
/// the "synced Xm ago" anchor visible during a background refresh so switching
/// feels like navigation, not a command re-run.
fn queue_sync_text(
    mode: Mode,
    all_repos: bool,
    syncing: bool,
    last_synced: Option<DateTime<Local>>,
) -> String {
    let loading = match (mode, all_repos) {
        (Mode::Authored, true) => "Loading pull requests involving you…",
        (Mode::Authored, false) => "Loading your open PRs…",
        (Mode::Review, _) => "Loading review queue…",
    };
    match (syncing, last_synced) {
        (true, None) | (false, None) => loading.to_string(),
        (true, Some(t)) => {
            let verb = match (mode, all_repos) {
                (Mode::Authored, true) => "Updating involving PRs…",
                (Mode::Authored, false) => "Updating your PRs…",
                (Mode::Review, _) => "Updating review queue…",
            };
            format!("{verb} · synced {}", relative(t))
        }
        (false, Some(t)) => format!("synced {}", relative(t)),
    }
}

/// The centered body copy shown before a queue's first rows ever arrive.
fn queue_loading_text(mode: Mode, all_repos: bool) -> &'static str {
    match (mode, all_repos) {
        (Mode::Authored, true) => "Loading pull requests involving you…",
        (Mode::Authored, false) => "Loading your open PRs…",
        (Mode::Review, _) => "Loading review queue…",
    }
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

impl RootView {
    fn render_setup(&self, setup: SetupStatus, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (title, detail, show_auth_command) = match &setup {
            SetupStatus::Checking => (
                "Checking GitHub CLI…".to_owned(),
                "PR Marmot uses your existing GitHub CLI session. No credentials are stored by the app."
                    .to_owned(),
                false,
            ),
            SetupStatus::MissingGh => (
                "Install the GitHub CLI".to_owned(),
                "The `gh` command was not found. Install it with `brew install gh` or from cli.github.com, then authenticate."
                    .to_owned(),
                true,
            ),
            SetupStatus::NotAuthenticated => (
                "Sign in with GitHub CLI".to_owned(),
                "Run the command below in Terminal, complete GitHub's sign-in flow, then retry. PR Marmot never displays or stores your token."
                    .to_owned(),
                true,
            ),
            SetupStatus::Network(_) => (
                "GitHub could not be reached".to_owned(),
                "Check your connection and GitHub CLI access, then retry.".to_owned(),
                false,
            ),
            SetupStatus::Failed(message) => (
                "GitHub setup failed".to_owned(),
                message.clone(),
                false,
            ),
            SetupStatus::Ready => (String::new(), String::new(), false),
        };
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_3()
            .px_4()
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_size(px(18.))
                    .child(title),
            )
            .child(
                div()
                    .max_w(px(560.))
                    .text_color(theme.muted_foreground)
                    .child(detail),
            )
            .when(show_auth_command, |view| {
                view.child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(
                            div()
                                .px_3()
                                .py_2()
                                .rounded(px(5.))
                                .bg(theme.muted)
                                .font_family("monospace")
                                .child("gh auth login"),
                        )
                        .child(
                            Button::new("copy-gh-auth")
                                .small()
                                .label("Copy command")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(
                                        "gh auth login".to_owned(),
                                    ));
                                    this.show_feedback("Command copied", cx);
                                })),
                        ),
                )
            })
            .when(setup != SetupStatus::Checking, |view| {
                view.child(
                    Button::new("retry-setup")
                        .primary()
                        .label("Retry")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.state.update(cx, |state, cx| state.validate_setup(cx));
                        })),
                )
            })
    }
}

/// "45s" / "3m" / "1h 5m" — compact, for the back-off retry countdown.
fn human_duration(secs: u64) -> String {
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m", secs / 60),
        _ => format!("{}h {}m", secs / 3600, (secs % 3600) / 60),
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
        if self.selected_row_url(cx).is_none() {
            self.details_open = false;
        }
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
            } else if let Some(secs) = s.backoff_remaining() {
                BodyState::Paused(format!("Paused — retrying in {}", human_duration(secs)))
            } else if let Some(err) = s.error.clone() {
                BodyState::Failed(format!("Couldn't load — {err}"))
            } else {
                BodyState::Loading(queue_loading_text(s.mode, s.scope.is_all()).to_string())
            }
        };

        v_flex()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .track_focus(&self.focus_handle)
            .when(self.details_open, |view| {
                view.key_context("PrmarmotDetails")
            })
            .on_action(cx.listener(|this, _: &CloseDetails, window, cx| {
                this.details_open = false;
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
            // cells at Size::Small density.
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .text_size(px(crate::design::TABLE_TEXT_PX))
                    .map(|this| match body {
                        // bordered defaults to TRUE — at full bleed the outer
                        // border + rounded corners fight the window edge
                        // (spec §5: header border is the only separator). Once
                        // loaded, the delegate's own render_empty shows the
                        // queue-specific "nothing here" — correct, because we
                        // now KNOW the queue is empty.
                        BodyState::Loaded if self.visible_count == 0 && self.filtering() => this
                            .child(
                                h_flex()
                                    .size_full()
                                    .justify_center()
                                    .text_color(theme.muted_foreground)
                                    .child(format!(
                                        "No loaded PRs match {} — clear the search or load more.",
                                        self.filter_summary()
                                    )),
                            ),
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
                                .child(div().max_w(px(640.)).text_color(theme.danger).child(text))
                                .child(
                                    Button::new("retry-board")
                                        .label("Retry")
                                        .tooltip("Retry loading this queue · r")
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.state.update(cx, |state, cx| state.refresh(cx));
                                            this.focus_handle.focus(window, cx);
                                        })),
                                ),
                        ),
                    }),
            )
            .when(self.details_open, |this| {
                this.child(self.render_details(cx))
            })
            .child(self.render_footer(cx))
            .children(dialog_layer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counts(need_you: usize, complete: bool, tracked: (usize, usize)) -> HeaderCounts {
        HeaderCounts {
            loaded: 56,
            truncated: true,
            need_you,
            need_you_complete: complete,
            tracked_loaded: tracked.0,
            tracked_total: tracked.1,
        }
    }

    #[test]
    fn the_header_says_who_needs_you_in_plain_words() {
        let line = |c: HeaderCounts| header_counts(&c).0;
        assert_eq!(
            line(counts(0, true, (0, 0))),
            "56 loaded · partial results · nothing needs you"
        );
        assert_eq!(
            line(counts(1, false, (2, 2))),
            "56 loaded · partial results · 1 needs you so far · 2 watched/snoozed"
        );
        assert_eq!(
            line(counts(3, true, (50, 64))),
            "56 loaded · partial results · 3 need you · 50 of 64 watched/snoozed"
        );
        let (_, tip) = header_counts(&counts(3, false, (2, 2)));
        assert_eq!(
            tip,
            "56 PRs loaded in this view; GitHub has more (Load more).\n\
             3 need you so far: your PRs that need action plus reviews requested from you, \
             not counting snoozed ones. The Dock badge shows the same number. Only the \
             queues loaded since launch are counted.\n\
             2 watched/snoozed: watched and snoozed PRs, refreshed with this view (up to 50 \
             each time)."
        );
    }
}
