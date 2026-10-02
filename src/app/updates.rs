//! Checking for a newer release, starting the update, and the banners that
//! say so.

use super::*;

/// What the window knows about updates.
pub(super) struct Updates {
    /// The daily automatic check is on (Settings).
    pub(super) automatic: bool,
    pub(super) paths: crate::config::UpdatePaths,
    /// A newer release, once a check found one.
    pub(super) available: Option<AvailableUpdate>,
    /// Why the last check or update failed, for the banner.
    pub(super) error: Option<String>,
    pub(super) check_pending: bool,
    pub(super) starting: bool,
    pub(super) check_task: Option<gpui::Task<()>>,
}

#[derive(Clone)]
pub(super) struct AvailableUpdate {
    pub(super) version: StableVersion,
    pub(super) page_url: String,
    pub(super) channel: InstallChannel,
}

impl RootView {
    pub(super) fn start_update_check_loop(&mut self, cx: &mut Context<Self>) {
        self.updates.check_task = Some(cx.spawn(async move |this, cx| loop {
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

    pub(super) fn check_for_updates(&mut self, cx: &mut Context<Self>) {
        if !self.updates.automatic || self.updates.check_pending {
            return;
        }
        self.updates.check_pending = true;
        let state_path = self.updates.paths.check_state.clone();
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move {
                    let identity = crate::updates::ReleaseIdentity::new(crate::RELEASE_REPO)?;
                    // The GitHub CLI when it can answer, else GitHub directly
                    // without a token, so an install without `gh` still hears
                    // about releases.
                    let source = crate::updates::FallbackReleaseSource::new(
                        crate::updates::GhReleaseSource::new(
                            prmarmot_core::github::gh_cli::resolve_gh_path(),
                        ),
                        crate::updates::HttpReleaseSource::github(
                            &prmarmot_local::session::user_agent(
                                "prmarmot",
                                env!("CARGO_PKG_VERSION"),
                            ),
                        ),
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
                this.updates.check_pending = false;
                match outcome {
                    Ok((checked, channel)) => {
                        if let Some(CheckResult::Available {
                            version, page_url, ..
                        }) = check_result(&checked)
                        {
                            this.updates.available = Some(AvailableUpdate {
                                version: *version,
                                page_url: page_url.clone(),
                                channel: channel.unwrap_or(InstallChannel::Direct),
                            });
                        } else if matches!(
                            check_result(&checked),
                            Some(CheckResult::UpToDate { .. })
                        ) {
                            this.updates.available = None;
                        }
                    }
                    Err(error) => eprintln!("prmarmot: automatic update check failed: {error}"),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn begin_update(&mut self, window: &Window, cx: &mut Context<Self>) {
        let Some(available) = self.updates.available.clone() else {
            return;
        };
        match available.channel {
            InstallChannel::Direct => cx.open_url(&available.page_url),
            InstallChannel::Homebrew { brew_path, .. } => {
                if self.updates.starting {
                    return;
                }
                self.updates.starting = true;
                self.updates.error = None;
                let invocation = match crate::updates::UpgradeIdentity::new(
                    crate::CASK_TOKEN,
                    crate::APPLICATION_NAME,
                ) {
                    Ok(identity) => crate::updates::HelperInvocation {
                        parent_pid: std::process::id(),
                        brew_path,
                        identity,
                        receipt_path: self.updates.paths.upgrade_receipt.clone(),
                        lock_path: self.updates.paths.helper_lock.clone(),
                        open_path: PathBuf::from("/usr/bin/open"),
                    },
                    Err(error) => {
                        self.updates.starting = false;
                        self.updates.error = Some(error.to_string());
                        cx.notify();
                        return;
                    }
                };
                let executable = match std::env::current_exe() {
                    Ok(executable) => executable,
                    Err(error) => {
                        self.updates.starting = false;
                        self.updates.error = Some(format!("Could not start update: {error}"));
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
                            this.updates.starting = false;
                            this.updates.error = Some(format!("Could not start update: {error}"));
                            cx.notify();
                        }
                    });
                })
                .detach();
            }
        }
    }

    pub(super) fn render_update_banners(&self, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        // Core words the notice; the board below is still right, so it is
        // muted, not an error, and cannot be dismissed while it stays true.
        let state = self.state.read(cx);
        let access_notice = prmarmot_core::status::access_notice(&state.access);
        let reach_notice = state
            .token_reach_hint
            .then(|| prmarmot_core::status::pasted_token_reach_notice().to_owned());
        // All open: part of the search matched only the loaded PRs, and
        // GitHub has more, so a short list is not the repository's answer.
        let filter_notice = (state.mode == Mode::AllOpen
            && self.filtering()
            && state
                .total
                .is_some_and(|total| (state.rows.len() as u64) < total))
        .then(|| {
            all_open_local_filter_notice(
                &local_only_terms(&self.filter_text, &self.filter_chips, &state.rows_filter),
                state.rows.len(),
                state.total,
                state.pagination_can_load_more(),
            )
        })
        .flatten();
        v_flex()
            .flex_shrink_0()
            .children(
                access_notice
                    .into_iter()
                    .chain(reach_notice)
                    .chain(filter_notice)
                    .enumerate()
                    .map(|(index, notice)| {
                        let tooltip = notice.clone();
                        h_flex()
                            .px(px(crate::design::HEADER_PAD_X))
                            .py_1()
                            .bg(theme.muted)
                            .border_b_1()
                            .border_color(theme.border)
                            .child(
                                div()
                                    .id(("access-notice", index))
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(type_size::SMALL)
                                    .text_color(theme.muted_foreground)
                                    .child(notice)
                                    .tooltip(move |window, cx| {
                                        Tooltip::new(tooltip.clone()).build(window, cx)
                                    }),
                            )
                    }),
            )
            .when(!self.config_warnings.is_empty(), |banners| {
                let mut lines = v_flex().flex_1().min_w_0();
                for (index, (line, whole)) in
                    self.config_warnings.banner_lines().into_iter().enumerate()
                {
                    lines = lines.child(
                        div()
                            .id(("config-warning", index))
                            .min_w_0()
                            .truncate()
                            .text_size(type_size::SMALL)
                            .text_color(theme.warning)
                            .child(line)
                            .tooltip(move |window, cx| {
                                Tooltip::new(whole.clone()).build(window, cx)
                            }),
                    );
                }
                banners.child(
                    h_flex()
                        .items_start()
                        .px(px(crate::design::HEADER_PAD_X))
                        .py_1()
                        .gap_2()
                        .bg(theme.muted)
                        .border_b_1()
                        .border_color(theme.border)
                        .child(lines)
                        .child(
                            Button::new("dismiss-config-warning")
                                .small()
                                .ghost()
                                .label("Dismiss")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.config_warnings = Default::default();
                                    cx.notify();
                                })),
                        ),
                )
            })
            .when_some(self.updates.error.clone(), |banners, error| {
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
                                .text_size(type_size::SMALL)
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
                                    this.updates.error = None;
                                    cx.notify();
                                })),
                        ),
                )
            })
            .when_some(self.updates.available.clone(), |banners, update| {
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
                                .text_size(type_size::SMALL)
                                .font_weight(FontWeight::MEDIUM)
                                .child(format!("v{} available", update.version)),
                        )
                        .child(
                            Button::new("install-update")
                                .small()
                                .primary()
                                .label(if self.updates.starting {
                                    "Starting update…"
                                } else {
                                    "Update"
                                })
                                .disabled(self.updates.starting)
                                .on_click(
                                    cx.listener(|this, _, window, cx| {
                                        this.begin_update(window, cx)
                                    }),
                                ),
                        ),
                )
            })
    }
}
