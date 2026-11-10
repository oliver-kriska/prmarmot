//! Checking for a newer release, the banners that say so, and how to update:
//! the release page for a direct install, and for a Homebrew install the
//! command to run, which PR Marmot suggests and never runs itself.

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
    pub(super) check_task: Option<gpui::Task<()>>,
    /// What "Check for Updates…" shows, while its dialog is open. Shared
    /// rather than read from the view: the dialog is drawn inside the view's
    /// own render, where reading the view again would double-lease it.
    pub(super) manual: Option<std::rc::Rc<std::cell::RefCell<UpdatePrompt>>>,
}

/// The update dialog's copy of what it shows.
pub(super) struct UpdatePrompt {
    status: ManualCheck,
    available: Option<AvailableUpdate>,
}

/// The answer to "Check for Updates…".
#[derive(Clone)]
pub(super) enum ManualCheck {
    Checking,
    UpToDate {
        latest: StableVersion,
    },
    /// `Updates::available` holds the release.
    Available,
    /// Another PR Marmot process holds the check's lock.
    Busy,
    Failed(String),
}

/// One check, off the UI thread. `asked` skips the daily gate (the menu
/// command); the automatic check keeps it. The install channel is looked up
/// only when there is something to install.
fn run_check(
    state_path: PathBuf,
    asked: bool,
) -> Result<(AutomaticCheck, Option<InstallChannel>), crate::updates::UpdateError> {
    let identity = crate::updates::ReleaseIdentity::new(crate::RELEASE_REPO)?;
    // The GitHub CLI when it can answer, else GitHub directly without a
    // token, so an install without `gh` still hears about releases.
    let source = crate::updates::FallbackReleaseSource::new(
        crate::updates::GhReleaseSource::new(prmarmot_core::github::gh_cli::resolve_gh_path()),
        crate::updates::HttpReleaseSource::github(&prmarmot_local::session::user_agent(
            "prmarmot",
            env!("CARGO_PKG_VERSION"),
        )),
    );
    let checker = crate::updates::UpdateChecker::new(state_path, source);
    let checked = if asked {
        checker.check_now(unix_now(), env!("CARGO_PKG_VERSION"), &identity)?
    } else {
        checker.automatic_check(true, unix_now(), env!("CARGO_PKG_VERSION"), &identity)?
    };
    let channel = if check_result(&checked)
        .is_some_and(|result| matches!(result, CheckResult::Available { .. }))
    {
        // `scripts/demo.sh --update-homebrew`: the demo is never a cask
        // install, so it says it is one to show that banner and dialog.
        if std::env::var_os("PRMARMOT_DEMO_HOMEBREW").is_some() {
            return Ok((
                checked,
                Some(InstallChannel::Homebrew {
                    brew_path: PathBuf::from("/opt/homebrew/bin/brew"),
                    cask_root: PathBuf::new(),
                }),
            ));
        }
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
    Ok((checked, channel))
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
                .spawn(async move { run_check(state_path, false) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.updates.check_pending = false;
                match outcome {
                    Ok((checked, channel)) => this.record_check(&checked, channel),
                    Err(error) => eprintln!("prmarmot: automatic update check failed: {error}"),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// A newer release goes to the banner; an up-to-date answer clears it.
    fn record_check(&mut self, checked: &AutomaticCheck, channel: Option<InstallChannel>) {
        match check_result(checked) {
            Some(CheckResult::Available {
                version, page_url, ..
            }) => {
                self.updates.available = Some(AvailableUpdate {
                    version: *version,
                    page_url: page_url.clone(),
                    channel: channel.unwrap_or(InstallChannel::Direct),
                });
            }
            Some(CheckResult::UpToDate { .. }) => self.updates.available = None,
            _ => {}
        }
        self.sync_update_prompt();
    }

    /// Copy the banner's state into an open update dialog.
    pub(super) fn sync_update_prompt(&self) {
        if let Some(prompt) = &self.updates.manual {
            let mut prompt = prompt.borrow_mut();
            prompt.available = self.updates.available.clone();
        }
    }

    /// "Check for Updates…": ask GitHub now, whatever the daily gate and the
    /// automatic-check setting say, and show the answer in a dialog.
    pub(super) fn check_for_updates_now(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let checking = self
            .updates
            .manual
            .as_ref()
            .is_some_and(|prompt| matches!(prompt.borrow().status, ManualCheck::Checking));
        if !checking {
            self.updates.manual = Some(std::rc::Rc::new(std::cell::RefCell::new(UpdatePrompt {
                status: ManualCheck::Checking,
                available: self.updates.available.clone(),
            })));
            let state_path = self.updates.paths.check_state.clone();
            cx.spawn(async move |this, cx| {
                let outcome = cx
                    .background_executor()
                    .spawn(async move { run_check(state_path, true) })
                    .await;
                let _ = this.update(cx, |this, cx| {
                    let answer = match outcome {
                        Ok((checked, channel)) => {
                            this.record_check(&checked, channel);
                            match check_result(&checked) {
                                Some(CheckResult::Available { .. }) => ManualCheck::Available,
                                Some(CheckResult::UpToDate { latest }) => {
                                    ManualCheck::UpToDate { latest: *latest }
                                }
                                Some(CheckResult::Failed { message }) => {
                                    ManualCheck::Failed(message.clone())
                                }
                                None => ManualCheck::Busy,
                            }
                        }
                        Err(error) => ManualCheck::Failed(error.to_string()),
                    };
                    // The dialog was closed meanwhile: the banner has the news.
                    if let Some(prompt) = &this.updates.manual {
                        prompt.borrow_mut().status = answer;
                    }
                    cx.notify();
                    // The dialog is drawn by the component layer from a
                    // shared cell, which view retention does not track.
                    cx.refresh_windows();
                });
            })
            .detach();
        }
        if !window.has_active_dialog(cx) {
            self.open_update_dialog(window, cx);
        }
    }

    fn open_update_dialog(&self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(prompt) = self.updates.manual.clone() else {
            return;
        };
        let view = cx.entity().downgrade();
        let focus = self.table.focus_handle(cx);
        window.open_dialog(cx, move |dialog, _, cx| {
            let shown = prompt.borrow();
            let current = env!("CARGO_PKG_VERSION");
            let available = shown.available.clone();
            let up_to_date = || {
                (
                    "PR Marmot is up to date".to_owned(),
                    format!("Version {current} is the newest release."),
                )
            };
            let (title, body) = match &shown.status {
                ManualCheck::Checking => (
                    "Checking for updates…".to_owned(),
                    format!("Asking GitHub for the newest release. You have version {current}."),
                ),
                ManualCheck::Available => match &available {
                    Some(update) => (
                        "A new version is available".to_owned(),
                        match update.channel {
                            InstallChannel::Homebrew { .. } => format!(
                                "PR Marmot {} is available. You have version {current}. \
                                 Homebrew manages this copy: quit PR Marmot, then run \
                                 this in Terminal:",
                                update.version
                            ),
                            InstallChannel::Direct => format!(
                                "PR Marmot {} is available. You have version {current}.",
                                update.version
                            ),
                        },
                    ),
                    None => up_to_date(),
                },
                ManualCheck::UpToDate { latest }
                    if StableVersion::parse(current).is_ok_and(|now| *latest < now) =>
                {
                    (
                        "PR Marmot is up to date".to_owned(),
                        format!("Version {current} is newer than the latest release, {latest}."),
                    )
                }
                ManualCheck::UpToDate { .. } => up_to_date(),
                ManualCheck::Busy => (
                    "Already checking".to_owned(),
                    "Another PR Marmot window is checking for updates. Try again in a moment."
                        .to_owned(),
                ),
                ManualCheck::Failed(message) => {
                    ("Couldn't check for updates".to_owned(), message.clone())
                }
            };
            let offer = matches!(shown.status, ManualCheck::Available)
                .then_some(available)
                .flatten();
            drop(shown);
            let command = offer
                .as_ref()
                .filter(|update| matches!(update.channel, InstallChannel::Homebrew { .. }))
                .map(|_| homebrew_update_command());
            let footer = match offer {
                Some(update) => {
                    let homebrew = matches!(update.channel, InstallChannel::Homebrew { .. });
                    let view = view.clone();
                    h_flex()
                        .gap_2()
                        .child(
                            crate::skin::quiet(Button::new("update-later"), cx)
                                .label("Later")
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(
                            Button::new("update-now")
                                .primary()
                                .label(if homebrew { "Copy command" } else { "Download" })
                                .on_click(move |_, window, cx| {
                                    window.close_dialog(cx);
                                    let _ = view.update(cx, |this, cx| this.begin_update(cx));
                                }),
                        )
                }
                None => h_flex().child(
                    crate::skin::quiet(Button::new("update-done"), cx)
                        .label("OK")
                        .on_click(|_, window, cx| window.close_dialog(cx)),
                ),
            };
            let view = view.clone();
            let focus = focus.clone();
            dialog
                .title(title)
                .w(px(420.))
                .close_button(false)
                .child(div().text_size(type_size::BODY).child(body))
                .when_some(command, |dialog, command| {
                    dialog.child(
                        div()
                            .mt_2()
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .bg(cx.theme().muted)
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_size(type_size::SMALL)
                            .child(SelectableText::new("update-command", command)),
                    )
                })
                .child(h_flex().mt_3().justify_end().child(footer))
                .on_close(move |_, window, cx| {
                    let _ = view.update(cx, |this, _| this.updates.manual = None);
                    focus.focus(window, cx);
                })
        });
    }

    /// The banner's and the dialog's button. A direct install opens the
    /// release page. A Homebrew install gets the command on the clipboard:
    /// Homebrew owns that copy, and an upgrade run from inside the app it
    /// replaces is one the person can't watch, answer or retry.
    pub(super) fn begin_update(&mut self, cx: &mut Context<Self>) {
        let Some(available) = self.updates.available.clone() else {
            return;
        };
        match available.channel {
            InstallChannel::Direct => {
                crate::open::open_web_url(cx, &available.page_url);
            }
            InstallChannel::Homebrew { .. } => {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(homebrew_update_command()));
                self.show_feedback(
                    "Copied — quit PR Marmot, then paste the command in Terminal",
                    cx,
                );
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
                        .child({
                            let homebrew =
                                matches!(update.channel, InstallChannel::Homebrew { .. });
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_size(type_size::SMALL)
                                .font_weight(FontWeight::MEDIUM)
                                .child(if homebrew {
                                    format!(
                                        "v{} available · quit PR Marmot, then update with Homebrew",
                                        update.version
                                    )
                                } else {
                                    format!("v{} available", update.version)
                                })
                        })
                        .child(
                            Button::new("install-update")
                                .small()
                                .primary()
                                .label(match update.channel {
                                    InstallChannel::Homebrew { .. } => "Copy command",
                                    InstallChannel::Direct => "Download",
                                })
                                .on_click(cx.listener(|this, _, _, cx| this.begin_update(cx))),
                        ),
                )
            })
    }
}

/// What a Homebrew install runs to update: refresh Homebrew's index first,
/// because on its own it refreshes at most once a day and would call a
/// release from this morning "already installed"; then reopen the app.
pub(super) fn homebrew_update_command() -> String {
    format!(
        "brew update && brew upgrade --cask {} && open -a {}",
        crate::CASK_TOKEN,
        crate::APPLICATION_NAME
    )
}
