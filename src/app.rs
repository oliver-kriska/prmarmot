//! Root view: header (repo, counts, sync + rate-limit status) over the board
//! table, the auto-refresh loop, and the keyboard/mouse actions.

use std::collections::HashMap;
use std::time::Duration;

use chrono::{DateTime, Local};
use gpui::prelude::FluentBuilder;
use gpui::{
    div, px, App, AppContext, ClipboardItem, Context, Entity, FocusHandle, Focusable, FontWeight,
    InteractiveElement, IntoElement, KeyDownEvent, ParentElement, Pixels, Render, Styled, Window,
};
use gpui_component::button::Button;
use gpui_component::select::{SearchableVec, Select, SelectEvent, SelectState};
use gpui_component::tab::{Tab, TabBar};
use gpui_component::table::{Column, DataTable, TableEvent, TableState};
use gpui_component::{h_flex, v_flex, ActiveTheme, IndexPath, Sizable, TitleBar};
use prboard_core::board::Mode;

use crate::state::{relative, AppState};
use crate::table::{columns_for, BoardTableDelegate, TableWidthClass};
use crate::theme::ThemePref;

/// Startup decisions resolved in `main` (CLI + env + config file).
pub struct Launch {
    pub theme: ThemePref,
    pub refresh: Duration,
    /// Repo-picker entries; the active repo is always among them.
    pub repos: Vec<String>,
}

/// Persist the current window size so the next launch opens the same way.
/// Only the plain-windowed size — maximized/fullscreen store their restore
/// size, which is what we'd want back anyway.
fn save_window_size(window: &Window) {
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
    discovering_repos: bool,
    repo_status: String,
    focus_handle: FocusHandle,
    seen_generation: u64,
    theme_pref: ThemePref,
    refresh: Duration,
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
                    let repo = repo.clone();
                    crate::config::persist_str("repo", &repo);
                    // View-local navigation state belongs to the old repo — its
                    // cached rows and per-queue selections are meaningless for a
                    // different repo (critique #4). Clear before switching.
                    this.selections.clear();
                    this.pending_restore = None;
                    this.last_selected = 0;
                    this.state.update(cx, |s, cx| s.switch_repo(repo, cx));
                },
            )
            .detach();
            select
        };

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
                let rows = state.read(cx).rows.clone();

                // Resolve selection by PR identity (URL), never by raw index:
                // on a switch, the queue's remembered PR; on a plain refresh,
                // the PR currently selected (so a reorder/insert/remove keeps
                // the same PR highlighted, or clears if it's gone).
                let switching = this.pending_restore.take();
                let current_url = this.table.read(cx).selected_row().and_then(|ix| {
                    this.table
                        .read(cx)
                        .delegate()
                        .row(ix)
                        .map(|r| r.url.clone())
                });
                let target_url = match &switching {
                    Some(mode) => this.selections.get(mode).cloned(),
                    None => current_url,
                };
                let scroll = switching.is_some();

                this.table.update(cx, |table, cx| {
                    table.delegate_mut().set_rows(rows);
                    table.refresh(cx);
                    match target_url.and_then(|u| table.delegate().display_index_of_url(&u)) {
                        Some(ix) => {
                            table.set_selected_row(ix, cx);
                            if scroll {
                                table.scroll_to_row(ix, cx);
                            }
                        }
                        None => table.clear_selection(cx),
                    }
                });
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
            discovering_repos: false,
            repo_status: String::new(),
            focus_handle: cx.focus_handle(),
            seen_generation: 0,
            theme_pref,
            refresh: launch.refresh,
            selections: HashMap::new(),
            pending_restore: None,
            last_selected: 0,
            width_class: initial_class,
            viewport_width: initial_width,
            col_overrides: HashMap::new(),
        };
        this.start_refresh_loop(cx);
        this.discover_repos(window, cx);
        this
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

    fn start_refresh_loop(&self, cx: &mut Context<Self>) {
        let state = self.state.clone();
        state.update(cx, |s, cx| s.refresh(cx));
        let interval = self.refresh;
        let state = state.downgrade();
        cx.spawn(async move |_this, cx| {
            loop {
                cx.background_executor().timer(interval).await;
                // Rate-limit gating and in-flight dedup live inside refresh().
                if state.update(cx, |s, cx| s.refresh(cx)).is_err() {
                    break; // app is shutting down
                }
            }
        })
        .detach();
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
        // Search input owns text keys. Typing a repository containing q/r/t
        // must not quit, refresh, or change the app's theme.
        if !self.table.focus_handle(cx).contains_focused(window, cx) {
            return;
        }
        let key = event.keystroke.key.as_str();
        let platform = event.keystroke.modifiers.platform;
        match key {
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
            "enter" | "o" if !platform => {
                if let Some(url) = self.selected_row_url(cx) {
                    cx.open_url(&url);
                }
            }
            "y" if !platform => {
                if let Some(url) = self.selected_row_url(cx) {
                    cx.write_to_clipboard(ClipboardItem::new_string(url));
                }
            }
            _ => {}
        }
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
                    .gap_3()
                    .text_size(px(12.))
                    .text_color(theme.muted_foreground)
                    .child(div().text_color(status_color).child(status_text))
                    .when_some(budget, |this, b| this.child(b)),
            )
    }

    fn render_footer(&self, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let theme_label = format!("theme ({})", self.theme_pref.label());
        let hints: Vec<(&str, String)> = vec![
            ("↑↓", "select".into()),
            ("⏎", "open".into()),
            ("y", "copy".into()),
            ("1/2", "views".into()),
            ("r", "refresh".into()),
            ("t", theme_label),
            ("q", "quit".into()),
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
        bar.child(div().flex_1())
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
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
                BodyState::Failed(format!("Couldn't load — {err}  ·  press r to retry"))
            } else {
                BodyState::Loading(queue_loading_text(s.mode).to_string())
            }
        };

        v_flex()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::handle_key_down))
            .child(TitleBar::new().child(self.render_header(cx)))
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
                        BodyState::Loaded => this.child(
                            DataTable::new(&self.table)
                                .small()
                                .stripe(true)
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
                            h_flex()
                                .size_full()
                                .justify_center()
                                .text_color(theme.danger)
                                .child(text),
                        ),
                    }),
            )
            .child(self.render_footer(cx))
    }
}
