//! Root view: header (repo, counts, sync + rate-limit status) over the board
//! table, the auto-refresh loop, and the keyboard/mouse actions.

use std::collections::HashMap;
use std::time::Duration;

use chrono::{DateTime, Local};
use gpui::prelude::FluentBuilder;
use gpui::{
    div, px, App, AppContext, ClipboardItem, Context, Entity, FocusHandle, Focusable, FontWeight,
    InteractiveElement, IntoElement, KeyBinding, KeyDownEvent, ParentElement, Pixels, Render,
    StatefulInteractiveElement, Styled, Window,
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
use prboard_core::board::Mode;

use crate::state::{relative, AppState};
use crate::table::{columns_for, detail_text, matches_filter, BoardTableDelegate, TableWidthClass};
use crate::theme::ThemePref;

gpui::actions!(prboard, [CloseDetails]);

/// Startup decisions resolved in `main` (CLI + env + config file).
pub struct Launch {
    pub theme: ThemePref,
    pub refresh: Duration,
    /// Repo-picker entries; the active repo is always among them.
    pub repos: Vec<String>,
    pub pinned_repos: Vec<String>,
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
    filter_text: String,
    visible_count: usize,
    details_open: bool,
    focus_handle: FocusHandle,
    seen_generation: u64,
    theme_pref: ThemePref,
    refresh: Duration,
    refresh_task: Option<gpui::Task<()>>,
    feedback: Option<&'static str>,
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
    /// user dragged. Bounded by construction: 2 modes × 3 classes = 6 entries.
    col_overrides: HashMap<(Mode, TableWidthClass), Vec<Pixels>>,
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
            Some("PrboardDetails > DataTable"),
        )]);
        let mode = state.read(cx).mode;
        // Size the columns to the window the moment we open, so the first frame
        // is already responsive (no default-width flash).
        let initial_width: f32 = window.viewport_size().width.into();
        let initial_class = TableWidthClass::from_width(initial_width);
        let table = cx.new(|cx| {
            let mut delegate = BoardTableDelegate::new(mode);
            delegate.set_columns(columns_for(mode, initial_class, initial_width));
            TableState::new(delegate, window, cx)
                .sortable(false)
                .col_movable(false)
                .col_resizable(true)
                .row_selectable(true)
        });

        let repo_select = {
            let current = state.read(cx).repo.clone();
            let selected = launch
                .repos
                .iter()
                .position(|r| *r == current)
                .map(IndexPath::new);
            let select = cx.new(|cx| {
                SelectState::new(
                    SearchableVec::new(launch.repos.clone()),
                    selected,
                    window,
                    cx,
                )
                .searchable(true)
            });
            cx.subscribe(
                &select,
                |this: &mut Self, _, event: &SelectEvent<SearchableVec<String>>, cx| {
                    let SelectEvent::Confirm(Some(repo)) = event else {
                        return;
                    };
                    this.select_repo(repo.clone(), cx);
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

        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Filter loaded PRs…  /"));
        cx.subscribe(&search, |this: &mut Self, input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.filter_text = input.read(cx).value().to_string();
                this.sync_table(cx);
            }
        })
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
        cx.spawn(async move |_this, cx| loop {
            cx.background_executor()
                .timer(Duration::from_secs(60))
                .await;
            let Some(view) = ticker.upgrade() else { break };
            view.update(cx, |_, cx| cx.notify());
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
        };
        this.state.update(cx, |state, cx| state.refresh(cx));
        this.start_refresh_loop(cx);
        this.discover_repos(window, cx);
        this
    }

    fn sync_table(&mut self, cx: &mut Context<Self>) {
        let rows: Vec<_> = self
            .state
            .read(cx)
            .rows
            .iter()
            .filter(|row| matches_filter(row, &self.filter_text))
            .cloned()
            .collect();
        self.visible_count = rows.len();
        let switching = self.pending_restore.take();
        let target_url = match switching {
            Some(mode) => self.selections.get(&mode).cloned(),
            None => self.selected_row_url(cx),
        };
        self.table.update(cx, |table, cx| {
            table.delegate_mut().set_rows(rows);
            table.refresh(cx);
            if let Some(ix) = target_url.and_then(|u| table.delegate().display_index_of_url(&u)) {
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

    fn select_repo(&mut self, repo: String, cx: &mut Context<Self>) {
        if self.state.read(cx).repo.eq_ignore_ascii_case(&repo) {
            return;
        }
        crate::config::persist_str("repo", &repo);
        self.selections.clear();
        self.pending_restore = None;
        self.last_selected = 0;
        self.details_open = false;
        self.state.update(cx, |s, cx| s.switch_repo(repo, cx));
    }

    fn toggle_pin(&mut self, cx: &mut Context<Self>) {
        let repo = self.state.read(cx).repo.clone();
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
                this.theme_pref = ThemePref::resolve(file.theme.as_deref());
                this.theme_pref.apply(window, cx);
                let refresh = crate::state::refresh_interval(file.refresh_secs);
                if this.refresh != refresh {
                    this.refresh = refresh;
                    this.start_refresh_loop(cx);
                }
                this.state.update(cx, |state, cx| {
                    state.apply_config(crate::board_config(&file), cx)
                });
                window.close_dialog(cx);
                this.table.focus_handle(cx).focus(window, cx);
                this.show_feedback("Settings saved", cx);
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
    fn show_feedback(&mut self, message: &'static str, cx: &mut Context<Self>) {
        self.feedback = Some(message);
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
                .spawn(async { prboard_core::github::gh_cli::list_repos() })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.discovering_repos = false;
                match result {
                    Ok(discovery) => {
                        let current = this.state.read(cx).repo.clone();
                        let mut repos = this.configured_repos.clone();
                        repos.extend(discovery.repos);
                        repos.push(current.clone());
                        repos.sort_unstable_by_key(|r| r.to_lowercase());
                        repos.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
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

    fn selected_row_url(&self, cx: &App) -> Option<String> {
        let table = self.table.read(cx);
        let row_ix = table.selected_row()?;
        table.delegate().row(row_ix).map(|r| r.url.clone())
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
        let cols = self.columns_for_current(mode);
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
    fn columns_for_current(&self, mode: Mode) -> Vec<Column> {
        let mut cols = columns_for(mode, self.width_class, self.viewport_width);
        if let Some(widths) = self.col_overrides.get(&(mode, self.width_class)) {
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
        if !class_changed && self.col_overrides.contains_key(&(mode, new_class)) {
            return; // manual layout stands within its width class
        }
        let cols = self.columns_for_current(mode);
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
            self.col_overrides.insert((mode, self.width_class), widths);
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
                self.search_open = true;
                self.search.focus_handle(cx).focus(window, cx);
                cx.notify();
                cx.stop_propagation();
            }
            "space" if table_focused => self.toggle_details(window, cx),
            "q" => {
                save_window_size(window);
                cx.quit();
            }
            "r" => self.state.update(cx, |s, cx| s.refresh(cx)),
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
            "y" if !platform => {
                if let Some(url) = self.selected_row_url(cx) {
                    cx.write_to_clipboard(ClipboardItem::new_string(url));
                    self.show_feedback("PR URL copied", cx);
                }
            }
            _ => {}
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
        let pinned = self
            .pinned_repos
            .iter()
            .any(|pin| pin.eq_ignore_ascii_case(&state.repo));
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
                                .when(repo.eq_ignore_ascii_case(&state.repo), |button| {
                                    button
                                        .bg(cx.theme().accent)
                                        .text_color(cx.theme().accent_foreground)
                                })
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.select_repo(target.clone(), cx);
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
                            !pinned && self.pinned_repos.len() >= crate::config::MAX_PINNED_REPOS,
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_pin(cx))),
                )
                .child(
                    Button::new("open-search")
                        .small()
                        .label("Search · /")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.search_open = true;
                            this.search.focus_handle(cx).focus(window, cx);
                            cx.notify();
                        })),
                )
            })
            .when(self.search_open, |bar| {
                bar.child(
                    div().w(px(280.)).child(
                        Input::new(&self.search)
                            .small()
                            .aria_label("Filter loaded PRs"),
                    ),
                )
            })
            .when(self.search_open, |bar| {
                bar.child(
                    Button::new("clear-filter")
                        .small()
                        .label("Close search")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.search
                                .update(cx, |input, cx| input.set_value("", window, cx));
                            this.filter_text.clear();
                            this.search_open = false;
                            this.sync_table(cx);
                            this.table.focus_handle(cx).focus(window, cx);
                        })),
                )
            })
            .when(!self.filter_text.trim().is_empty(), |bar| {
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
                                    for (label, text) in [
                                        ("Copy PR number", format!("#{}", copy_row.number)),
                                        ("Copy title", copy_row.title.clone()),
                                        ("Copy URL", copy_row.url.clone()),
                                        (
                                            "Copy all details",
                                            format!(
                                                "#{} {}\n{}\n\n{}",
                                                copy_row.number,
                                                copy_row.title,
                                                copy_row.url,
                                                detail_text(&copy_row)
                                            ),
                                        ),
                                    ] {
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
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .child(SelectableText::new("detail-body", detail_text(&row))),
                            ),
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
        // titlebar room for the future `/` search field.
        let counts = format!(
            "{} shown{}",
            state.rows.len(),
            if state.truncated {
                " · partial results"
            } else {
                ""
            }
        );
        // Status priority: a hard error wins; then a rate-limit back-off (so a
        // switch into a paused window shows "paused", not a permanent
        // "Loading…"); otherwise the queue-specific sync line. Static text only
        // — a spinner would defeat the idle-GPU half of the spike gate.
        let (status_text, status_color) = if let Some(err) = state.error.clone() {
            (err, theme.danger)
        } else if let Some(secs) = state.backoff_remaining() {
            (
                format!("paused · retry in {}", human_duration(secs)),
                theme.warning,
            )
        } else {
            (
                queue_sync_text(state.mode, state.syncing, state.last_synced),
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
            .child(Tab::new().label("My PRs").w(px(104.)).font_weight(
                if state.mode == Mode::Authored {
                    FontWeight::SEMIBOLD
                } else {
                    FontWeight::MEDIUM
                },
            ))
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
                    .min_w_0()
                    .flex_1()
                    .truncate()
                    .text_size(px(13.))
                    .text_color(theme.muted_foreground)
                    .child(counts),
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

    fn render_footer(&self, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let hints: Vec<(&str, String)> = vec![
            ("↑↓", "select".into()),
            ("⏎", "open".into()),
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
        .when_some(self.feedback, |bar, message| {
            bar.child(
                div()
                    .text_size(px(12.))
                    .text_color(theme.foreground)
                    .child(message),
            )
        })
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

    fn show_shortcuts(&self, window: &mut Window, cx: &mut Context<Self>) {
        let focus = self.table.focus_handle(cx);
        window.open_dialog(cx, move |dialog, window, cx| {
            let mut rows = v_flex().id("shortcut-list").overflow_y_scroll()
                .max_h((window.viewport_size().height - px(300.)).min(px(340.)))
                .gap_2().text_size(px(13.));
            for (label, keys) in [
                ("Select a PR", "↑ / ↓"),
                ("Open selected PR", "Enter / o"),
                ("Copy selected PR URL", "y"),
                ("My PRs / Review queue", "1 / 2"),
                ("Switch queue", "v"),
                ("Refresh", "r"),
                ("Search loaded PRs", "/"),
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
                    .child("Typing in search, the repository picker, or Settings never triggers dashboard shortcuts. Arrow keys, Enter, and Space act on the focused control."))
                .child(h_flex().mt_3().justify_end().child(Button::new("close-shortcuts")
                    .label("Done").on_click(|_, window, cx| window.close_dialog(cx))))
                .on_close(move |_, window, cx| focus.focus(window, cx))
        });
    }
}

/// The header's right-side status line, specific to the active queue. Keeps
/// the "synced Xm ago" anchor visible during a background refresh so switching
/// feels like navigation, not a command re-run.
fn queue_sync_text(mode: Mode, syncing: bool, last_synced: Option<DateTime<Local>>) -> String {
    let loading = match mode {
        Mode::Authored => "Loading your open PRs…",
        Mode::Review => "Loading review queue…",
    };
    match (syncing, last_synced) {
        (true, None) | (false, None) => loading.to_string(),
        (true, Some(t)) => {
            let verb = match mode {
                Mode::Authored => "Updating your PRs…",
                Mode::Review => "Updating review queue…",
            };
            format!("{verb} · synced {}", relative(t))
        }
        (false, Some(t)) => format!("synced {}", relative(t)),
    }
}

/// The centered body copy shown before a queue's first rows ever arrive.
fn queue_loading_text(mode: Mode) -> &'static str {
    match mode {
        Mode::Authored => "Loading your open PRs…",
        Mode::Review => "Loading review queue…",
    }
}

/// What the table area shows, derived from `AppState` truth (`last_synced` /
/// back-off / error) rather than the row `generation` — so an unseen queue or a
/// first-fetch failure never renders a contradictory "empty" or "Loading…".
enum BodyState {
    Loaded,
    Loading(String),
    Paused(String),
    Failed(String),
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
            if s.last_synced.is_some() {
                BodyState::Loaded
            } else if let Some(secs) = s.backoff_remaining() {
                BodyState::Paused(format!("Paused — retrying in {}", human_duration(secs)))
            } else if let Some(err) = s.error.clone() {
                BodyState::Failed(format!("Couldn't load — {err}"))
            } else {
                BodyState::Loading(queue_loading_text(s.mode).to_string())
            }
        };

        v_flex()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .track_focus(&self.focus_handle)
            .when(self.details_open, |view| view.key_context("PrboardDetails"))
            .on_action(cx.listener(|this, _: &CloseDetails, window, cx| {
                this.details_open = false;
                this.table.focus_handle(cx).focus(window, cx);
                cx.notify();
            }))
            .on_key_down(cx.listener(Self::handle_key_down))
            .child(TitleBar::new().child(self.render_header(cx)))
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
                        BodyState::Loaded
                            if self.visible_count == 0 && !self.filter_text.trim().is_empty() =>
                        {
                            this.child(
                                h_flex()
                                    .size_full()
                                    .justify_center()
                                    .text_color(theme.muted_foreground)
                                    .child(
                                        "No matching loaded PRs — clear the filter or load more.",
                                    ),
                            )
                        }
                        BodyState::Loaded => this.child(
                            DataTable::new(&self.table)
                                .small()
                                .stripe(false)
                                .bordered(false),
                        ),
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
