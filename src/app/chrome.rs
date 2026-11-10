//! The window's chrome around the board: the tools bar, the header with its
//! counts, the footer, the search box and the shortcuts sheet.

use super::*;

impl RootView {
    pub(super) fn render_tools(&self, cx: &Context<Self>) -> impl IntoElement {
        let state = self.state.read(cx);
        // The pill and the Snoozed header's chevron are one switch.
        let snoozed_shown = !self.collapsed_here(state).contains(&SectionKind::Snoozed);
        let current_repo = state.scope.repository();
        let pinned = self
            .pinned_repos
            .iter()
            .any(|pin| current_repo.is_some_and(|repo| pin.eq_ignore_ascii_case(repo)));
        let glacier = crate::skin::glacier(cx);
        h_flex()
            .px(px(16.))
            .py(px(6.))
            .gap_2()
            .border_b_1()
            .border_color(cx.theme().border)
            // The app root paints Glacier's chrome once, including its tint.
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
                                    .text_size(type_size::SMALL)
                                    .text_color(cx.theme().muted_foreground)
                                    .child("Pin your frequent repositories"),
                            )
                        })
                        .children(self.pinned_repos.iter().enumerate().map(|(ix, repo)| {
                            let target = repo.clone();
                            let current = current_repo
                                .is_some_and(|current| repo.eq_ignore_ascii_case(current));
                            Button::new(("pinned-repo", ix))
                                .small()
                                // A child element, not a label: name it.
                                .accessibility_label(repo.clone())
                                .child(div().max_w(px(160.)).truncate().child(repo.clone()))
                                // Ghosts, the current repository in the one
                                // "on" recipe (both skins since the
                                // 2026-10-21 critique).
                                .map(|button| crate::skin::on_control(button, current, cx))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.select_scope(BoardScope::Repository(target.clone()), cx);
                                    this.repo_select.update(cx, |select, cx| {
                                        select.set_selected_value(&target, window, cx);
                                    });
                                    this.table.focus_handle(cx).focus(window, cx);
                                }))
                        })),
                )
                // With all repositories there is nothing to pin, so the
                // toggle is not drawn rather than drawn disabled (critique
                // 2026-10-21: an empty grey box in the toolbar).
                .when(current_repo.is_some(), |bar| {
                    bar.child(
                        Button::new("pin-current")
                            .small()
                            .toggled(pinned)
                            // A toggle in the "on" recipe, with its mark.
                            .map(|button| {
                                crate::skin::on_control(button, pinned, cx)
                                    .accessibility_label(if pinned { "Pinned" } else { "Pin" })
                                    .child(
                                        h_flex()
                                            .gap_1p5()
                                            .items_center()
                                            .when(pinned, |row| {
                                                row.child(
                                                    div().text_size(type_size::CAPTION).child("✓"),
                                                )
                                            })
                                            .child(if pinned { "Pinned" } else { "Pin" }),
                                    )
                            })
                            .tooltip(if pinned {
                                "Unpin this repository".to_owned()
                            } else if self.pinned_repos.len() >= crate::config::MAX_PINNED_REPOS {
                                pin_limit_text()
                            } else {
                                "Pin this repository".to_owned()
                            })
                            .disabled(
                                !pinned
                                    && self.pinned_repos.len() >= crate::config::MAX_PINNED_REPOS,
                            )
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.toggle_pin(cx);
                                this.table.focus_handle(cx).focus(window, cx);
                            })),
                    )
                })
                .child(
                    Button::new("open-search")
                        .small()
                        // The key as a keycap beside the word, in both skins
                        // (one keycap everywhere, 2026-10-21).
                        .when(glacier, |button| button.outline())
                        .child(hinted_label("Search", "/", cx))
                        .accessibility_label("Search")
                        .map(|button| crate::skin::quiet(button, cx))
                        .tooltip(match (state.mode, cfg!(target_os = "macos")) {
                            (Mode::AllOpen, true) => "Filter open PRs (/ or ⌘F). Click a label or author, or type label: or author:, to search the whole repository; other words filter the loaded PRs.",
                            (Mode::AllOpen, false) => "Filter open PRs (/ or Ctrl F). Click a label or author, or type label: or author:, to search the whole repository; other words filter the loaded PRs.",
                            (_, true) => "Filter loaded PRs (/ or ⌘F). Type label:, author:, repo:, is:stale, or is:agent, or click a label, author, or repository.",
                            (_, false) => "Filter loaded PRs (/ or Ctrl F). Type label:, author:, repo:, is:stale, or is:agent, or click a label, author, or repository.",
                        })
                        .on_click(cx.listener(|this, _, window, cx| this.open_search(window, cx))),
                )
            })
            .when(self.search_open, |bar| {
                bar.child(self.render_search_box(cx))
            })
            .when(
                self.filtering() || self.changed_only || self.needs_you_only,
                |bar| {
                bar.child(
                    div()
                        .text_size(type_size::SMALL)
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
            // The Needs you filter is the header's "N need you" itself
            // (`needs_you_control`): one number in the chrome, not three.
            .child(
                view_toggle(
                    "changed-filter",
                    "Changed",
                    self.counts.changed,
                    self.changed_only,
                    Some(cx.theme().link),
                    cx,
                )
                .tooltip(changed_toggle_tooltip(
                    self.changed_only,
                    self.counts.changed,
                ))
                .on_click(cx.listener(|this, _, window, cx| {
                    this.changed_only = !this.changed_only;
                    this.sync_table(cx);
                    this.table.focus_handle(cx).focus(window, cx);
                })),
            )
            .child({
                let stale_on = self.stale_filtering();
                view_toggle(
                    "stale-filter",
                    STALE_TOGGLE_LABEL,
                    self.counts.stale,
                    stale_on,
                    None,
                    cx,
                )
                .tooltip(stale_toggle_tooltip(
                    stale_on,
                    self.counts.stale,
                    state.config.stale_after_days,
                ))
                .on_click(cx.listener(|this, _, window, cx| {
                    this.toggle_stale_filter(window, cx);
                }))
            })
            .child({
                let agent_on = self.agent_filtering();
                view_toggle(
                    "agent-filter",
                    AGENT_TOGGLE_LABEL,
                    self.counts.agent,
                    agent_on,
                    None,
                    cx,
                )
                .tooltip(agent_toggle_tooltip(agent_on, self.counts.agent))
                .on_click(cx.listener(|this, _, window, cx| {
                    this.toggle_agent_filter(window, cx);
                }))
            })
            .child(
                view_toggle(
                    "snoozed-toggle",
                    "Snoozed",
                    self.counts.snoozed,
                    snoozed_shown,
                    None,
                    cx,
                )
                .tooltip(snoozed_toggle_tooltip(snoozed_shown, self.counts.snoozed))
                .on_click(cx.listener(|this, _, window, cx| {
                    this.toggle_section(SectionKind::Snoozed, cx);
                    this.table.focus_handle(cx).focus(window, cx);
                })),
            )
            .when(state.mode == Mode::Review, |bar| {
                bar.child(
                    view_toggle(
                        "smallest-first",
                        "Smallest first",
                        0,
                        self.smallest_first,
                        None,
                        cx,
                    )
                    .tooltip(if self.smallest_first {
                        "Listing the smallest changes first. Click to list the longest wait first."
                    } else {
                        "List requested and available reviews by size: Small, then Medium, then Large"
                    })
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.smallest_first = !this.smallest_first;
                        this.sync_table(cx);
                        this.table.focus_handle(cx).focus(window, cx);
                    })),
                )
            })
            .when(self.search_open, |bar| bar.child(div().flex_1()))
            .when(state.truncated, |bar| {
                bar.child(
                    Button::new("load-more")
                        .small()
                        .map(|button| {
                            if crate::skin::glacier(cx) {
                                button.ghost()
                            } else {
                                crate::skin::quiet(button, cx)
                            }
                        })
                        .label(if state.syncing() {
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
                        .tooltip(REFRESH_NOTE)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.state.update(cx, |state, cx| state.load_more(cx));
                            this.table.focus_handle(cx).focus(window, cx);
                        })),
                )
            })
            // Details changes the layout, not the rows: set apart from the
            // filters.
            .child(
                div()
                    .w(px(1.))
                    .h(px(16.))
                    .flex_shrink_0()
                    .bg(cx.theme().border),
            )
            .child(
                Button::new("show-details")
                    .small()
                    .when(glacier, |button| button.outline())
                    .map(|button| {
                        if self.details_open {
                            button.label("Hide details")
                        } else {
                            button
                                .child(hinted_label("Details", "Space", cx))
                                .accessibility_label("Details")
                        }
                    })
                    .map(|button| crate::skin::quiet(button, cx))
                    .disabled(self.visible_count == 0)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.toggle_details(window, cx);
                    })),
            )
    }

    pub(super) fn render_freshness_notice(&self, cx: &Context<Self>) -> Option<impl IntoElement> {
        let state = self.state.read(cx);
        if state.setup != SetupStatus::Ready {
            return None;
        }
        let title = prmarmot_core::status::stale_board_notice(
            state.last_synced.map(|t| (Local::now() - t).num_seconds()),
            self.refresh.as_secs(),
        )?;
        let paused = state.pause_text();
        let syncing = state.syncing();
        let can_refresh = !syncing && paused.is_none();
        let progress = if syncing {
            "Updating from GitHub…".to_owned()
        } else if let Some(paused) = paused {
            format!("Refresh paused: {paused}.")
        } else if state.error.is_some() {
            match state.retry_remaining() {
                Some(secs) => format!(
                    "Couldn't refresh from GitHub — retrying in {}.",
                    prmarmot_core::status::human_duration(secs)
                ),
                None => "Couldn't refresh from GitHub.".to_owned(),
            }
        } else {
            "Refresh to check GitHub.".to_owned()
        };
        let message = format!("{progress} PRs may have changed or merged.");
        let theme = cx.theme();
        Some(
            h_flex()
                .id("board-freshness-notice")
                .role(gpui::Role::Status)
                .aria_label(format!("{title}. {message}"))
                .flex_shrink_0()
                .px(px(crate::design::HEADER_PAD_X))
                .py_2()
                .gap_3()
                .border_l(px(3.))
                .border_color(theme.warning)
                .bg(theme.muted)
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_1()
                        .child(
                            div()
                                .text_size(type_size::BODY)
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(title),
                        )
                        .child(
                            div()
                                .text_size(type_size::SMALL)
                                .text_color(theme.muted_foreground)
                                .child(message),
                        ),
                )
                .when(can_refresh, |notice| {
                    notice.child(
                        Button::new("refresh-older-data")
                            .small()
                            .label("Refresh now")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.state.update(cx, |state, cx| state.refresh(cx));
                            })),
                    )
                }),
        )
    }

    pub(super) fn render_header(&self, cx: &Context<Self>) -> impl IntoElement {
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
            mode: state.mode,
            all_repos: state.scope.is_all(),
            need_you: self.counts.needs_you,
            badge: state.badge_count,
            badge_complete: state.badge_coverage_complete,
            followed: self.counts.followed,
            tracked_loaded: state.tracked_loaded,
            tracked_total: state.tracked_total,
            total: state.total,
            filtered: !state.rows_filter.is_empty(),
        });
        // Status priority: a hard error wins; then a rate-limit back-off (so a
        // switch into a paused window shows "paused", not a permanent
        // "Loading…"); otherwise the queue-specific sync line. Static text only
        // — a spinner would defeat the idle-GPU half of the spike gate.
        let (status_text, status_color) = if state.setup == SetupStatus::Checking {
            ("Checking GitHub CLI…".to_owned(), theme.muted_foreground)
        } else if state.setup != SetupStatus::Ready {
            ("GitHub setup required".to_owned(), theme.warning)
        } else if let Some(err) = state.error_text() {
            (err, theme.danger)
        } else if let Some(paused) = state.pause_text() {
            (paused, theme.warning)
        } else if let Some(progress) = state.fetch_progress_text() {
            (progress, theme.muted_foreground)
        } else {
            (
                queue_sync_text(
                    state.mode,
                    state.scope.is_all(),
                    state.syncing(),
                    state.last_synced,
                ),
                theme.muted_foreground,
            )
        };
        // Under a tenth of the hourly budget left, the readout turns the
        // warning colour well before fetching pauses at the reserve.
        let budget = state.rate.as_ref().map(|r| {
            let tip = prmarmot_core::status::api_budget_tooltip(r.remaining, r.limit);
            div()
                .id("api-budget")
                .flex_shrink_0()
                .when(r.is_low(), |this| this.text_color(theme.warning))
                .child(format!("API {}/{}", r.remaining, r.limit))
                .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
        });
        // The queue is a primary scope, not a hidden preference. A compact
        // toolbar tab view keeps every choice visible and the selected state
        // persistent (Apple HIG); equal widths prevent any queue from
        // appearing subordinate. Keyboard 1/2/3 and v remain accelerators.
        // All open needs one repository, so with all of them it stays in
        // place, disabled, and says why on hover rather than disappearing.
        let all_repos = state.scope.is_all();
        let view_switcher = if crate::skin::glacier(cx) {
            self.render_view_segments(state.mode, all_repos, cx)
                .into_any_element()
        } else {
            self.render_view_tabs(state.mode, all_repos, cx)
                .into_any_element()
        };

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
                    .text_size(type_size::BODY)
                    .text_color(theme.muted_foreground)
                    .font_features(crate::design::tabular_numbers())
                    .child(counts_line(
                        &counts,
                        needs_you_control(
                            self.counts.needs_you,
                            self.needs_you_only,
                            self.needs_you_danger,
                            cx,
                        )
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.needs_you_only = !this.needs_you_only;
                            this.sync_table(cx);
                            this.table.focus_handle(cx).focus(window, cx);
                        })),
                        cx,
                    ))
                    .tooltip(move |window, cx| Tooltip::new(counts_tip.clone()).build(window, cx)),
            )
            .child(
                h_flex()
                    .flex_shrink_0()
                    .max_w(px(self.viewport_width * 0.28))
                    .gap_3()
                    .text_size(type_size::SMALL)
                    .text_color(theme.muted_foreground)
                    .font_features(crate::design::tabular_numbers())
                    .child(
                        div()
                            .id("sync-status")
                            .min_w_0()
                            .truncate()
                            .text_color(status_color)
                            .child(status_text.clone())
                            .tooltip(move |window, cx| {
                                Tooltip::new(format!("{status_text}\n{SYNC_STATUS_NOTE}"))
                                    .build(window, cx)
                            }),
                    )
                    .when_some(budget, |this, b| this.child(b)),
            )
    }

    /// Classic's view switcher: gpui-component's segmented tab bar.
    fn render_view_tabs(&self, mode: Mode, all_repos: bool, cx: &Context<Self>) -> TabBar {
        let selected_mode = match mode {
            Mode::Authored => 0,
            Mode::Review => 1,
            Mode::AllOpen => 2,
        };
        TabBar::new("view-switcher")
            .small()
            .segmented()
            .selected_index(selected_mode)
            .child(
                Tab::new()
                    .label(prmarmot_core::status::view_title(
                        Mode::Authored,
                        all_repos,
                        false,
                    ))
                    .w(px(104.))
                    .font_weight(if mode == Mode::Authored {
                        FontWeight::SEMIBOLD
                    } else {
                        FontWeight::MEDIUM
                    }),
            )
            .child(Tab::new().label("Review queue").w(px(104.)).font_weight(
                if mode == Mode::Review {
                    FontWeight::SEMIBOLD
                } else {
                    FontWeight::MEDIUM
                },
            ))
            .child(
                Tab::new()
                    .label("All open")
                    .w(px(104.))
                    .disabled(all_repos)
                    .when(all_repos, |tab| {
                        tab.tooltip(|window, cx| {
                            Tooltip::new(all_open_needs_repository()).build(window, cx)
                        })
                    })
                    .font_weight(if mode == Mode::AllOpen {
                        FontWeight::SEMIBOLD
                    } else {
                        FontWeight::MEDIUM
                    }),
            )
            .on_click(cx.listener(|this, index: &usize, window, cx| {
                let mode = match index {
                    0 => Mode::Authored,
                    1 => Mode::Review,
                    _ => Mode::AllOpen,
                };
                this.select_mode(mode, cx);
                this.table.focus_handle(cx).focus(window, cx);
            }))
    }

    /// Glacier's view switcher: a track with three segments, the selected one
    /// raised — a lighter fill and a small shadow — in every colour set
    /// (critique C2: the framework's segmented tab paints the sheet's colour,
    /// which is darker than its track in the dark and reads sunken).
    fn render_view_segments(
        &self,
        mode: Mode,
        all_repos: bool,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let segment = |id: &'static str, label: SharedString, target: Mode, disabled: bool| {
            let selected = mode == target;
            Button::new(id)
                .small()
                .ghost()
                .toggled(selected)
                .w(px(104.))
                .label(label)
                .disabled(disabled)
                .font_weight(if selected {
                    FontWeight::SEMIBOLD
                } else {
                    FontWeight::MEDIUM
                })
                .text_color(if selected {
                    theme.tab_active_foreground
                } else {
                    theme.tab_foreground
                })
                .when(selected, |button| {
                    button
                        .bg(crate::skin::raised_tab_bg(cx))
                        .rounded(px(crate::skin::CONTROL_RADIUS - 2.))
                        .shadow_sm()
                })
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.select_mode(target, cx);
                    this.table.focus_handle(cx).focus(window, cx);
                }))
        };
        h_flex()
            .id("view-switcher")
            .p(px(2.))
            .gap(px(2.))
            .items_center()
            .rounded(px(crate::skin::CONTROL_RADIUS))
            .bg(theme.tab_bar_segmented)
            .child(segment(
                "view-authored",
                prmarmot_core::status::view_title(Mode::Authored, all_repos, false).into(),
                Mode::Authored,
                false,
            ))
            .child(segment(
                "view-review",
                "Review queue".into(),
                Mode::Review,
                false,
            ))
            .child(
                segment("view-all-open", "All open".into(), Mode::AllOpen, all_repos)
                    .when(all_repos, |button| {
                        button.tooltip(all_open_needs_repository())
                    }),
            )
    }

    pub(super) fn render_footer(&self, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        // With several rows selected, the keys act on all of them, and the
        // first "key" is the count.
        let selected = self.table.read(cx).delegate().multi_len();
        let hints: Vec<(String, String)> = if selected > 1 {
            vec![
                (
                    prmarmot_core::status::selected_count_text(selected),
                    "⇧↑↓ extend".into(),
                ),
                ("⏎".into(), "open all".into()),
                ("y".into(), "copy URLs".into()),
                ("Y".into(), "copy list".into()),
                ("w".into(), "watch".into()),
                ("s".into(), "snooze".into()),
                ("Esc".into(), "clear".into()),
            ]
        } else {
            vec![
                ("↑↓".into(), "select".into()),
                ("⏎".into(), "open".into()),
                ("Space".into(), "details".into()),
                ("w".into(), "watch".into()),
                ("s".into(), "snooze".into()),
                ("r".into(), "refresh".into()),
            ]
        };
        // Keycap legend (spec §6): reference material lives at the bottom,
        // status at the top — the gh-dash/native pattern. Glacier groups
        // the keycaps in one tray.
        let glacier = crate::skin::glacier(cx);
        let mut bar = h_flex()
            .flex_shrink_0()
            .px(px(crate::design::HEADER_PAD_X))
            .py(px(crate::design::FOOTER_PAD_Y))
            .gap_3()
            .items_center()
            .when(!glacier, |bar| bar.bg(theme.title_bar))
            .border_t_1()
            .border_color(theme.title_bar_border);
        let hint = |key: String, label: String| {
            h_flex()
                .gap_1()
                .items_center()
                .child(keycap(key, cx))
                .child(
                    div()
                        .text_size(type_size::SMALL)
                        .text_color(theme.muted_foreground)
                        .child(label),
                )
        };
        if glacier {
            let mut tray = h_flex()
                .h(px(crate::skin::CONTROL_HEIGHT))
                .px(px(8.))
                .gap_3()
                .items_center()
                .rounded(px(crate::skin::CONTROL_RADIUS))
                .bg(theme.secondary);
            for (key, label) in hints {
                tray = tray.child(hint(key, label));
            }
            bar = bar.child(tray);
        } else {
            for (key, label) in hints {
                bar = bar.child(hint(key, label));
            }
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
                    .text_size(type_size::SMALL)
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
                        .text_size(type_size::CAPTION)
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
                        .map(|button| crate::skin::quiet(button, cx))
                        .label("Enable notifications…")
                        .tooltip(error)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.notification_help_shown = false;
                            this.state.read(cx).check_notification_permission();
                        })),
                )
            },
        )
        // The repository count lives on the Repos button; the status text
        // says only what the button can't ("Finding…", a failure, a pin).
        .when(self.repo_status != self.repo_count, |bar| {
            bar.child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_size(type_size::CAPTION)
                    .text_color(theme.muted_foreground)
                    .child(self.repo_status.clone()),
            )
        })
        .child(
            Button::new("discover-repos")
                .small()
                .map(|button| {
                    if crate::skin::glacier(cx) {
                        button.ghost()
                    } else {
                        crate::skin::quiet(button, cx)
                    }
                })
                .label("Repos")
                .tooltip(if self.repo_count.is_empty() {
                    "Find the repositories you can read".to_owned()
                } else {
                    format!("{} · click to look again", self.repo_count)
                })
                .on_click(cx.listener(|this, _, window, cx| this.discover_repos(window, cx))),
        )
        .child(
            Button::new("configuration")
                .small()
                .map(|button| crate::skin::quiet(button, cx))
                .label("Settings")
                .tooltip("Reviewer suggestions, refresh interval and appearance")
                .on_click(cx.listener(|this, _, window, cx| this.show_config(window, cx))),
        )
    }

    /// One search box: a search glyph, the label tokens (each removable),
    /// the typed words, and one × that clears everything and closes it.
    pub(super) fn render_search_box(&self, cx: &Context<Self>) -> impl IntoElement {
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
                label_chip(theme, crate::skin::glacier(cx), cx)
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

    pub(super) fn show_shortcuts(&self, window: &mut Window, cx: &mut Context<Self>) {
        let focus = self.table.focus_handle(cx);
        let all_repos = self.state.read(cx).scope.is_all();
        window.open_dialog(cx, move |dialog, window, cx| {
            // Tall enough for the whole list on a laptop screen; the key
            // column keeps its width and the words give way.
            let mut rows = v_flex().id("shortcut-list").overflow_y_scroll()
                .max_h((window.viewport_size().height - px(240.)).min(px(600.)))
                .gap_2().text_size(type_size::BODY);
            let views = format!(
                "{} / Review queue / All open",
                prmarmot_core::status::view_title(Mode::Authored, all_repos, false)
            );
            for (label, keys) in [
                ("Select a PR", "↑ / ↓"),
                ("Select several PRs", if cfg!(target_os = "macos") { "⇧-click, ⌘-click, ⇧↑ / ⇧↓, ⌘A" } else { "Shift-click, Ctrl-click, Shift ↑ / ↓, Ctrl A" }),
                ("Open selected PRs", "Enter / o"),
                ("Copy selected PR URLs", "y"),
                ("Copy selected PRs as a list (one PR: its group)", "Y"),
                ("Watch / unwatch selected PRs", "w"),
                ("Snooze selected PRs", "s"),
                ("Pick a snooze choice in its dialog", "1 2 3 …"),
                ("Fold / unfold the selected section", "c"),
                (views.as_str(), "1 / 2 / 3"),
                ("Switch queue", "v"),
                ("Refresh", "r"),
                (
                    "Search loaded PRs",
                    if cfg!(target_os = "macos") { "/ or ⌘F" } else { "/ or Ctrl F" },
                ),
                ("Search one label, author, or repo", "label: author: repo:"),
                ("Search PRs waiting too long for a reviewer", "is:stale"),
                ("Search PRs a coding agent or bot opened, or a person", "is:agent is:human"),
                ("Remove the last search filter", "⌫ in empty search"),
                ("Toggle selected PR details", "Space"),
                ("Cycle theme", "t"),
                ("Settings", if cfg!(target_os = "macos") { "⌘," } else { "Ctrl ," }),
                ("Close dialog or details", "Esc"),
                ("Quit", if cfg!(target_os = "macos") { "⌘Q" } else { "Ctrl Q" }),
            ] {
                // A key is drawn as the footer's keycap; search grammar is
                // typed, not pressed, so it is drawn as text (Amp,
                // 2026-10-21: `is:agent is:human` is syntax, not a chord).
                let grammar = keys.contains(':');
                let keys: String = keys.to_owned();
                rows = rows.child(
                    h_flex().justify_between().gap_3()
                        .child(div().min_w_0().truncate().child(label.to_owned()))
                        .child(if grammar {
                            div().flex_shrink_0().text_size(type_size::SMALL)
                                .text_color(cx.theme().muted_foreground).child(keys)
                        } else {
                            keycap(keys, cx).flex_shrink_0()
                        }),
                );
            }
            let focus = focus.clone();
            dialog.title("Keyboard shortcuts").w(px(460.)).close_button(false).child(rows)
                .child(div().mt_3().text_size(type_size::SMALL).text_color(cx.theme().muted_foreground)
                    .child("Clicking a label, author, or repository in the table adds it to the search. Quote values with spaces: label:\"help wanted\"."))
                .child(div().mt_2().text_size(type_size::SMALL).text_color(cx.theme().muted_foreground)
                    .child("Typing in search, the repository picker, or Settings never triggers dashboard shortcuts. Arrow keys, Enter, and Space act on the focused control."))
                .child(h_flex().mt_3().justify_end().child(crate::skin::quiet(Button::new("close-shortcuts"), cx)
                    .label("Done").on_click(|_, window, cx| window.close_dialog(cx))))
                .on_close(move |_, window, cx| focus.focus(window, cx))
        });
    }

    /// PR Marmot → About PR Marmot: the version, and where the project lives.
    pub(super) fn show_about(&self, window: &mut Window, cx: &mut Context<Self>) {
        let focus = self.table.focus_handle(cx);
        window.open_dialog(cx, move |dialog, _, cx| {
            let link = |id: &'static str, label: &'static str, url: &'static str, cx: &App| {
                crate::skin::quiet(Button::new(id), cx)
                    .label(label)
                    .on_click(move |_, _, cx| {
                        crate::open::open_web_url(cx, url);
                    })
            };
            let focus = focus.clone();
            dialog
                .w(px(380.))
                .close_button(false)
                .child(
                    h_flex()
                        .gap_4()
                        .child(gpui::img("branding/mascot.png").w(px(58.)).h(px(64.)).flex_shrink_0())
                        .child(
                            v_flex()
                                .gap_1()
                                .child(
                                    div()
                                        .text_size(type_size::DISPLAY)
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child("PR Marmot"),
                                )
                                .child(
                                    div()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(format!("Version {}", env!("CARGO_PKG_VERSION"))),
                                ),
                        ),
                )
                .child(
                    div()
                        .mt_3()
                        .text_size(type_size::BODY)
                        .child("A desktop dashboard for GitHub pull requests. Free and open source under the MIT licence."),
                )
                .child(
                    h_flex()
                        .mt_3()
                        .gap_2()
                        .flex_wrap()
                        .child(link("about-website", "Website", crate::WEBSITE_URL, cx))
                        .child(link("about-source", "Source code", "https://github.com/oliver-kriska/prmarmot", cx))
                        .child(link("about-notes", "Release notes", crate::RELEASE_NOTES_URL, cx)),
                )
                .child(
                    h_flex().mt_3().justify_end().child(
                        Button::new("about-done")
                            .primary()
                            .label("Done")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    ),
                )
                .on_close(move |_, window, cx| focus.focus(window, cx))
        });
    }
}

/// A toolbar toggle with a fixed label, the count it applies to, and an
/// optional dot. When on it takes the accent, like the current pinned repo.
/// A keycap, as the footer and Glacier's hinted buttons draw one.
pub(super) fn keycap(key: impl Into<SharedString>, cx: &App) -> Div {
    let theme = cx.theme();
    let glacier = crate::skin::glacier(cx);
    div()
        .px_1()
        .rounded(px(3.))
        .bg(if glacier {
            theme.background
        } else {
            theme.muted
        })
        .border_1()
        .border_color(theme.border)
        .text_size(type_size::CAPTION)
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme.secondary_foreground)
        .child(key.into())
}

/// Glacier: a button's word with its key as a keycap beside it.
fn hinted_label(label: &'static str, key: &'static str, cx: &App) -> Div {
    h_flex()
        .gap_1p5()
        .items_center()
        .child(label)
        .child(keycap(key, cx))
}

/// Whether a segment of the header's count line is its "need you" phrase
/// (`nothing needs you`, `1 needs you`, `N need you`; core writes them).
fn is_need_you_segment(segment: &str) -> bool {
    segment.ends_with("need you") || segment.ends_with("needs you")
}

/// The header's "N need you" as the Needs you filter itself: a capsule in
/// the warning tone — the rows it counts are mostly amber — or the danger
/// tone while one of them is a danger row (critique D3), and when the
/// filter is on a mark and the link's edge, so "on" never rides on the
/// urgency colour alone. Built from the count, not the sentence, so "1
/// needs you" is a control too (Amp, 2026-10-21). With nothing needing
/// you it is plain muted text, still a control while the filter is on so
/// it can be cleared.
pub(super) fn needs_you_control(
    count: usize,
    on: bool,
    danger: bool,
    cx: &App,
) -> gpui::Stateful<Div> {
    let theme = cx.theme();
    let tone = if danger { theme.danger } else { theme.warning };
    let name: SharedString = format!("{NEEDS_YOU_TOGGLE_LABEL} ({count})").into();
    let phrase: SharedString = match count {
        0 => "nothing needs you".into(),
        1 => "1 needs you".into(),
        n => format!("{n} need you").into(),
    };
    let lit = count > 0;
    let tip = needs_you_toggle_tooltip(on, count);
    // A div, not a Button: the framework's button paints its variant's
    // text colour over the content, so the capsule's tone would be lost.
    h_flex()
        .id("needs-you-filter")
        .h(px(crate::skin::CAPSULE_HEIGHT))
        .px(px(crate::skin::CAPSULE_PAD_X))
        .gap_1p5()
        .items_center()
        .rounded(px(crate::skin::CAPSULE_RADIUS))
        .text_size(type_size::SMALL)
        .flex_shrink_0()
        .cursor_pointer()
        .when(lit, |capsule| {
            capsule
                .bg(crate::skin::capsule_bg(tone, cx))
                .text_color(tone)
                .font_weight(FontWeight::SEMIBOLD)
        })
        .when(!lit, |capsule| capsule.text_color(theme.muted_foreground))
        .when(on, |capsule| capsule.border_1().border_color(theme.link))
        .hover(|style| style.bg(theme.muted))
        // Exposed as the toggle it is, like the quick filters beside it.
        .role(gpui::Role::CheckBox)
        .aria_toggled(if on {
            gpui::Toggled::True
        } else {
            gpui::Toggled::False
        })
        .aria_label(name)
        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
        .when(on, |capsule| {
            capsule.child(div().text_size(type_size::CAPTION).child("✓"))
        })
        .child(phrase)
}

/// The header's count line with its "need you" segment replaced by the
/// control that filters to those rows, the other counts as text around it.
/// The control keeps its width; the words around it shorten with an
/// ellipsis.
fn counts_line(counts: &str, control: impl IntoElement, cx: &App) -> Div {
    let _ = cx;
    let segments: Vec<&str> = counts.split(" · ").collect();
    let control_at = segments
        .iter()
        .position(|segment| is_need_you_segment(segment));
    let mut control = Some(control);
    let mut line = h_flex()
        .w_full()
        .min_w_0()
        .items_center()
        .whitespace_nowrap();
    for (ix, segment) in segments.iter().enumerate() {
        if ix > 0 {
            line = line.child(div().flex_shrink_0().child(" · "));
        }
        // The control is taken only at its own segment: taking it for every
        // segment would drop it on the first and leave the words in its place.
        let control_here = (control_at == Some(ix)).then(|| control.take()).flatten();
        match control_here {
            Some(control) => line = line.child(control),
            // The words give way before the control does.
            None => line = line.child(div().min_w_0().truncate().child(segment.to_string())),
        }
    }
    line
}

pub(super) fn view_toggle(
    id: &'static str,
    label: &'static str,
    count: usize,
    on: bool,
    dot: Option<gpui::Hsla>,
    cx: &App,
) -> Button {
    let theme = cx.theme();
    // The one "on" recipe (`skin::on_control`: a ghost at rest, the link's
    // tint under the text's own ink when on — link ink on that fill falls
    // under AA in the light sets), in both skins since the 2026-10-21
    // critique (Classic's ten bordered boxes ranked nothing).
    let count_color = if on {
        theme.foreground
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
        .map(|button| crate::skin::on_control(button, on, cx))
        .accessibility_label(name)
        .child(
            h_flex()
                .gap_1p5()
                .items_center()
                // On is said by a mark too, not by colour alone.
                .when(on, |row| {
                    row.child(div().text_size(type_size::CAPTION).child("✓"))
                })
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
                            .text_size(type_size::CAPTION)
                            .text_color(count_color)
                            .child(count.to_string()),
                    )
                }),
        )
}

/// The header's count line and the tooltip that explains it, with the badge
/// called by its desktop name.
pub(super) fn header_counts(c: &HeaderCounts) -> (String, String) {
    core_header_counts(c, BadgeName::Dock)
}

/// [`core_queue_sync_text`] with a local timestamp rather than an elapsed count.
pub(super) fn queue_sync_text(
    mode: Mode,
    all_repos: bool,
    syncing: bool,
    last_synced: Option<DateTime<Local>>,
) -> String {
    core_queue_sync_text(
        mode,
        all_repos,
        syncing,
        last_synced.map(|t| (Local::now() - t).num_seconds()),
    )
}
