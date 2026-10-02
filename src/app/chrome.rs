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
                                    .text_size(type_size::SMALL)
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
                            "Unpin this repository".to_owned()
                        } else if self.pinned_repos.len() >= crate::config::MAX_PINNED_REPOS {
                            pin_limit_text()
                        } else {
                            "Pin this repository".to_owned()
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
            .child(
                view_toggle(
                    "needs-you-filter",
                    NEEDS_YOU_TOGGLE_LABEL,
                    self.counts.needs_you,
                    self.needs_you_only,
                    None,
                    cx,
                )
                .tooltip(needs_you_toggle_tooltip(
                    self.needs_you_only,
                    self.counts.needs_you,
                ))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.needs_you_only = !this.needs_you_only;
                    this.sync_table(cx);
                })),
            )
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
                .on_click(cx.listener(|this, _, _, cx| {
                    this.changed_only = !this.changed_only;
                    this.sync_table(cx);
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
                .on_click(cx.listener(|this, _, _, cx| {
                    this.toggle_section(SectionKind::Snoozed, cx);
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
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.smallest_first = !this.smallest_first;
                        this.sync_table(cx);
                    })),
                )
            })
            .when(self.search_open, |bar| bar.child(div().flex_1()))
            .when(state.truncated, |bar| {
                bar.child(
                    Button::new("load-more")
                        .small()
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
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.state.update(cx, |state, cx| state.load_more(cx))
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
        } else if let Some(err) = state.error.clone() {
            (err, theme.danger)
        } else if let Some(paused) = state.pause_text() {
            (paused, theme.warning)
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
            div()
                .flex_shrink_0()
                .when(r.is_low(), |this| this.text_color(theme.warning))
                .child(format!("API {}/{}", r.remaining, r.limit))
        });
        let selected_mode = match state.mode {
            Mode::Authored => 0,
            Mode::Review => 1,
            Mode::AllOpen => 2,
        };
        // The queue is a primary scope, not a hidden preference. A compact
        // toolbar tab view keeps every choice visible and the selected state
        // persistent (Apple HIG); equal widths prevent any queue from
        // appearing subordinate. Keyboard 1/2/3 and v remain accelerators.
        // All open needs one repository, so with all of them it stays in
        // place, disabled, and says why on hover rather than disappearing.
        let all_repos = state.scope.is_all();
        let view_switcher = TabBar::new("view-switcher")
            .small()
            .segmented()
            .selected_index(selected_mode)
            .child(
                Tab::new()
                    .label(prmarmot_core::status::view_title(
                        Mode::Authored,
                        state.scope.is_all(),
                        false,
                    ))
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
                    .font_weight(if state.mode == Mode::AllOpen {
                        FontWeight::SEMIBOLD
                    } else {
                        FontWeight::MEDIUM
                    }),
            )
            .on_click(cx.listener(|this, index: &usize, _, cx| {
                let mode = match index {
                    0 => Mode::Authored,
                    1 => Mode::Review,
                    _ => Mode::AllOpen,
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
                    .text_size(type_size::BODY)
                    .text_color(theme.muted_foreground)
                    .font_features(crate::design::tabular_numbers())
                    .child(counts)
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
                            .text_size(type_size::CAPTION)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.secondary_foreground)
                            .child(key),
                    )
                    .child(
                        div()
                            .text_size(type_size::SMALL)
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
                .text_size(type_size::CAPTION)
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

    pub(super) fn show_shortcuts(&self, window: &mut Window, cx: &mut Context<Self>) {
        let focus = self.table.focus_handle(cx);
        window.open_dialog(cx, move |dialog, window, cx| {
            let mut rows = v_flex().id("shortcut-list").overflow_y_scroll()
                .max_h((window.viewport_size().height - px(300.)).min(px(450.)))
                .gap_2().text_size(type_size::BODY);
            for (label, keys) in [
                ("Select a PR", "↑ / ↓"),
                ("Select several PRs", if cfg!(target_os = "macos") { "⇧-click, ⌘-click, ⇧↑ / ⇧↓, ⌘A" } else { "Shift-click, Ctrl-click, Shift ↑ / ↓, Ctrl A" }),
                ("Open selected PRs", "Enter / o"),
                ("Copy selected PR URLs", "y"),
                ("Copy selected PRs as a list (one PR: its group)", "Y"),
                ("Watch / unwatch selected PRs", "w"),
                ("Snooze selected PRs", "s"),
                ("Fold / unfold the selected section", "c"),
                ("My PRs / Review queue / All open", "1 / 2 / 3"),
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
                .child(div().mt_3().text_size(type_size::SMALL).text_color(cx.theme().muted_foreground)
                    .child("Clicking a label, author, or repository in the table adds it to the search. Quote values with spaces: label:\"help wanted\"."))
                .child(div().mt_2().text_size(type_size::SMALL).text_color(cx.theme().muted_foreground)
                    .child("Typing in search, the repository picker, or Settings never triggers dashboard shortcuts. Arrow keys, Enter, and Space act on the focused control."))
                .child(h_flex().mt_3().justify_end().child(Button::new("close-shortcuts")
                    .label("Done").on_click(|_, window, cx| window.close_dialog(cx))))
                .on_close(move |_, window, cx| focus.focus(window, cx))
        });
    }
}

/// A toolbar toggle with a fixed label, the count it applies to, and an
/// optional dot. When on it takes the accent, like the current pinned repo.
pub(super) fn view_toggle(
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
        .when(on, |button| {
            button.bg(theme.accent).text_color(theme.accent_foreground)
        })
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
