//! Compact editable settings form hosted by the application's dialog shell.

use gpui::prelude::FluentBuilder;
use gpui::{
    div, img, px, App, AppContext, ClipboardItem, Context, Entity, EventEmitter, FontWeight,
    InteractiveElement, IntoElement, ParentElement, Render, ScrollHandle,
    StatefulInteractiveElement, Styled, Window,
};
use gpui_component::button::{Button, ButtonGroup, ButtonVariants};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{
    h_flex, v_flex, ActiveTheme, Disableable, IconName, Selectable, Sizable, WindowExt,
};
use prmarmot_core::board::IssueLinkRule;
use prmarmot_core::layout::{section_name, section_views, SectionOrder};

use prmarmot_local::auth::{token_store, TokenKind};
use prmarmot_local::config::AuthSettings;
use prmarmot_local::session::Connection;

use crate::config::{self, SettingsUpdate};
use crate::theme::ThemePref;

/// A token PR Marmot stored on this Mac, in words.
#[derive(Debug, Clone, PartialEq, Eq)]
struct StoredAccount {
    who: String,
    /// "signed in with GitHub" or "a personal access token".
    how: &'static str,
    /// Where the token lives: "the macOS keychain".
    place: String,
}

fn stored_account(auth: &AuthSettings) -> Option<StoredAccount> {
    let store = token_store(auth.store);
    let stored = store.load(&auth.host).ok().flatten()?;
    Some(StoredAccount {
        who: stored.login.unwrap_or_else(|| "this account".into()),
        how: match stored.kind {
            TokenKind::Device => "signed in with GitHub",
            TokenKind::Token => "a personal access token",
        },
        place: store.describe(),
    })
}

/// The Account block's sentence: which sign-in is live, and whether a stored
/// token sits unused beside the GitHub CLI login. `live` is what the app
/// connected with; `None` before it has. `inline` is a `PRMARMOT_TOKEN`.
fn account_detail(
    live: Option<(Connection, &str)>,
    stored: Option<&StoredAccount>,
    inline: bool,
    host: &str,
) -> String {
    match (live, stored) {
        // PRMARMOT_TOKEN comes before anything stored, so a stored token
        // beside it isn't the one in use.
        (Some((Connection::Token, login)), _) if inline => {
            format!("Using the token from PRMARMOT_TOKEN as {login}.")
        }
        (Some((Connection::GhCli, login)), None) => {
            format!("Using your GitHub CLI login as {login}.")
        }
        (Some((Connection::GhCli, login)), Some(stored)) => format!(
            "Using your GitHub CLI login as {login}. The token for {} stored in {} is unused \
             while the GitHub CLI is signed in; Disconnect removes it.",
            stored.who, stored.place
        ),
        (Some((_, login)), Some(stored)) => format!(
            "Using a stored token as {login} ({}). Token in {}.",
            stored.how, stored.place
        ),
        // Disconnected while that token was in use: this session still holds
        // it, but nothing is stored any more.
        (Some((_, login)), None) => format!(
            "Signed out on this Mac; PR Marmot asks {login} to sign in again at the next refresh."
        ),
        (None, Some(stored)) => format!(
            "A token for {} ({}) is stored in {}.",
            stored.who, stored.how, stored.place
        ),
        (None, None) => format!("Not signed in to {host}."),
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SettingsSaved;

impl EventEmitter<SettingsSaved> for SettingsView {}

#[derive(Default)]
struct EnvOverrides {
    reviewers: Option<String>,
    refresh: Option<u64>,
    theme: Option<String>,
    issue_link: Option<(String, String)>,
}

impl EnvOverrides {
    fn current() -> Self {
        let reviewers = std::env::var("PRMARMOT_DEFAULT_REVIEWERS")
            .ok()
            .filter(|value| !value.is_empty());
        let refresh = std::env::var("PRMARMOT_REFRESH_SECS")
            .ok()
            .and_then(|value| value.parse().ok());
        let theme = std::env::var("PRMARMOT_THEME").ok();
        let issue_link = match (
            std::env::var("PRMARMOT_ISSUE_PATTERN"),
            std::env::var("PRMARMOT_ISSUE_URL_TEMPLATE"),
        ) {
            (Ok(pattern), Ok(template)) => Some((pattern, template)),
            _ => None,
        };
        Self {
            reviewers,
            refresh,
            theme,
            issue_link,
        }
    }
}

pub struct SettingsView {
    reviewers: Entity<InputState>,
    refresh: Entity<InputState>,
    issue_pattern: Entity<InputState>,
    issue_url: Entity<InputState>,
    theme: ThemePref,
    notifications: bool,
    notification_sound: bool,
    notify_all_needs_action: bool,
    dock_badge: bool,
    automatic_update_checks: bool,
    /// The order being edited, and the one the file holds: saving writes the
    /// order only when they differ, so a hand-written list stays as written.
    section_order: SectionOrder,
    saved_section_order: SectionOrder,
    advanced: bool,
    error: Option<String>,
    error_field: Option<&'static str>,
    path_copied: bool,
    scroll: ScrollHandle,
    env: EnvOverrides,
    path: String,
    /// The GitHub host and token store this install is configured for.
    auth: AuthSettings,
    /// A token stored on this Mac, whether or not it is the one in use.
    account: Option<StoredAccount>,
    /// The sign-in the app connected with, and its login.
    live: Option<(Connection, String)>,
    /// What happened after a Disconnect.
    account_message: Option<String>,
}

impl SettingsView {
    pub fn new(
        live: Option<(Connection, String)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let file = config::load();
        let env = EnvOverrides::current();

        let reviewers = env
            .reviewers
            .clone()
            .unwrap_or_else(|| file.default_reviewers.join(", "));
        let refresh = env
            .refresh
            .or(file.refresh_secs)
            .unwrap_or(300)
            .max(30)
            .to_string();
        let theme = resolve_theme(env.theme.as_deref().or(file.theme.as_deref()));
        let auth = {
            let mut warnings = Vec::new();
            prmarmot_local::config::auth_settings(&file, None, None, &mut warnings)
        };
        let account = stored_account(&auth);
        let section_order = prmarmot_local::config::section_order(&file);
        let issue = env.issue_link.clone().or_else(|| {
            file.issue_link
                .map(|rule| (rule.pattern, rule.url_template))
        });
        let (pattern, url) = issue.unwrap_or_default();

        let this = Self {
            reviewers: cx.new(|cx| InputState::new(window, cx).default_value(reviewers)),
            refresh: cx.new(|cx| InputState::new(window, cx).default_value(refresh)),
            issue_pattern: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("PROJ-[0-9]+")
                    .default_value(pattern)
            }),
            issue_url: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("https://tracker.example.com/issues/{id}")
                    .default_value(url)
            }),
            theme,
            notifications: file.notifications,
            notification_sound: file.notification_sound,
            notify_all_needs_action: file.notify_all_needs_action,
            dock_badge: file.dock_badge,
            automatic_update_checks: file.automatic_update_checks,
            saved_section_order: section_order.clone(),
            section_order,
            advanced: false,
            error: None,
            error_field: None,
            path_copied: false,
            scroll: ScrollHandle::new(),
            env,
            path: config::config_path().display().to_string(),
            auth,
            account,
            live,
            account_message: None,
        };
        for (input, field) in [
            (&this.reviewers, "Reviewer suggestions"),
            (&this.refresh, "Refresh interval"),
            (&this.issue_pattern, "Issue ID regular expression"),
            (&this.issue_url, "Issue URL template"),
        ] {
            cx.subscribe(input, move |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) && this.error_field == Some(field) {
                    this.error = None;
                    this.error_field = None;
                    cx.notify();
                }
            })
            .detach();
        }
        this
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.error_field = None;
        let reviewers_text = self.reviewers.read(cx).value().to_string();
        let refresh_text = self.refresh.read(cx).value().to_string();
        let pattern = self.issue_pattern.read(cx).value().trim().to_owned();
        let url = self.issue_url.read(cx).value().trim().to_owned();

        let reviewers = if self.env.reviewers.is_some() {
            Vec::new()
        } else {
            match validate_reviewers(&reviewers_text) {
                Ok(value) => value,
                Err(error) => {
                    return self.show_invalid("Reviewer suggestions", error, window, cx);
                }
            }
        };
        let refresh = if let Some(value) = self.env.refresh {
            value
        } else {
            match validate_refresh(&refresh_text) {
                Ok(value) => value,
                Err(error) => {
                    return self.show_invalid("Refresh interval", error, window, cx);
                }
            }
        };
        let issue_link = if self.env.issue_link.is_some() {
            None
        } else {
            match validate_issue_link(&pattern, &url) {
                Ok(value) => value,
                Err(error) => {
                    self.advanced = true;
                    return self.show_invalid(error.field, error.message, window, cx);
                }
            }
        };

        let update = SettingsUpdate {
            default_reviewers: self.env.reviewers.is_none().then_some(reviewers),
            refresh_secs: self.env.refresh.is_none().then_some(refresh),
            theme: self
                .env
                .theme
                .is_none()
                .then(|| self.theme.label().to_owned()),
            issue_link: self.env.issue_link.is_none().then_some(issue_link),
            notifications: Some(self.notifications),
            notification_sound: Some(self.notification_sound),
            notify_all_needs_action: Some(self.notify_all_needs_action),
            dock_badge: Some(self.dock_badge),
            automatic_update_checks: Some(self.automatic_update_checks),
            section_order: (self.section_order != self.saved_section_order)
                .then(|| self.section_order.clone()),
        };
        match config::save_settings(&update) {
            Ok(()) => {
                self.error = None;
                cx.emit(SettingsSaved);
            }
            Err(error) => self.show_error(error, cx),
        }
    }

    fn show_error(&mut self, error: String, cx: &mut Context<Self>) {
        self.error = Some(error);
        cx.notify();
    }

    fn show_invalid(
        &mut self,
        field: &'static str,
        error: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.error_field = Some(field);
        // Indices are direct children of settings-scroll (the header and
        // Account come first); Advanced fields are siblings so either input
        // can be revealed independently.
        let (input, index) = match field {
            "Reviewer suggestions" => (&self.reviewers, 2),
            "Refresh interval" => (&self.refresh, 4),
            "Issue ID regular expression" => (&self.issue_pattern, 14),
            _ => (&self.issue_url, 15),
        };
        input.update(cx, |input, cx| input.focus(window, cx));
        self.scroll.scroll_to_top_of_item(index);
        self.show_error(error, cx);
    }

    /// Account block: which GitHub host, who is signed in here, where the
    /// token lives, and a way to forget it.
    fn render_account(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let detail = account_detail(
            self.live
                .as_ref()
                .map(|(via, login)| (*via, login.as_str())),
            self.account.as_ref(),
            self.auth.inline_token.is_some(),
            &self.auth.host,
        );
        v_flex()
            .gap_1()
            .child(
                h_flex().justify_between().child("Account").child(
                    div()
                        .text_size(px(11.))
                        .text_color(muted)
                        .child(self.auth.host.clone()),
                ),
            )
            .child(div().text_size(px(12.)).text_color(muted).child(detail))
            .when(self.account.is_some(), |block| {
                block.child(
                    Button::new("settings-disconnect")
                        .small()
                        .label("Disconnect")
                        .on_click(cx.listener(|this, _, _, cx| this.disconnect(cx))),
                )
            })
            .when_some(self.account_message.clone(), |block, message| {
                block.child(div().text_size(px(12.)).text_color(muted).child(message))
            })
    }

    /// Forget the token on this Mac. Revoking the grant at GitHub needs a
    /// client secret PR Marmot deliberately does not have, so we say so rather
    /// than pretend the app can do it.
    fn disconnect(&mut self, cx: &mut Context<Self>) {
        match token_store(self.auth.store).delete(&self.auth.host) {
            Ok(()) => {
                self.account = None;
                let what = match self.live {
                    Some((Connection::GhCli, _)) => {
                        "Removed the unused token; PR Marmot keeps using your GitHub CLI login."
                    }
                    _ => "Signed out on this Mac.",
                };
                self.account_message = Some(format!(
                    "{what} To revoke PR Marmot's access at GitHub, visit \
                     https://{}/settings/applications.",
                    self.auth.host
                ));
            }
            Err(message) => self.account_message = Some(message),
        }
        cx.notify();
    }

    /// Section order: one list for every view, each section moved with its up
    /// and down buttons. The names and where each shows come from core, as
    /// the iPad's list does.
    fn render_section_order(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let last = self.section_order.kinds().len().saturating_sub(1);
        v_flex()
            .gap_1()
            .child(
                h_flex().justify_between().child("Section order").child(
                    Button::new("settings-section-order-reset")
                        .small()
                        .ghost()
                        .label("Reset to default")
                        .disabled(self.section_order.is_default())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.section_order = SectionOrder::default();
                            cx.notify();
                        })),
                ),
            )
            .child(div().text_size(px(12.)).text_color(muted).child(
                "The order sections come in, the same in every view; each view shows the ones it \
                 has. Saved as section_order in config.toml.",
            ))
            .children(
                self.section_order
                    .kinds()
                    .iter()
                    .copied()
                    .enumerate()
                    .map(|(ix, kind)| {
                        let name = section_name(kind);
                        h_flex()
                            .gap_2()
                            .child(
                                div()
                                    .w(px(14.))
                                    .text_color(muted)
                                    .child((ix + 1).to_string()),
                            )
                            .child(div().child(name))
                            .child(
                                div()
                                    .flex_1()
                                    .text_size(px(11.))
                                    .text_color(muted)
                                    .child(section_views(kind)),
                            )
                            .child(
                                Button::new(("settings-section-up", ix))
                                    .small()
                                    .ghost()
                                    .icon(IconName::ArrowUp)
                                    .tooltip(format!("Move {name} up"))
                                    .disabled(ix == 0)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.section_order = this.section_order.moved(kind, true);
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new(("settings-section-down", ix))
                                    .small()
                                    .ghost()
                                    .icon(IconName::ArrowDown)
                                    .tooltip(format!("Move {name} down"))
                                    .disabled(ix == last)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.section_order = this.section_order.moved(kind, false);
                                        cx.notify();
                                    })),
                            )
                    }),
            )
    }

    fn field(
        &self,
        label: &'static str,
        input: &Entity<InputState>,
        env_name: Option<&'static str>,
        cx: &App,
    ) -> impl IntoElement {
        v_flex()
            .gap_1()
            .child(
                v_flex()
                    .gap_1()
                    .child(label)
                    .when_some(env_name, |row, name| {
                        row.child(
                            div()
                                .text_size(px(11.))
                                .text_color(cx.theme().muted_foreground)
                                .child(format!("Controlled by {name}")),
                        )
                    }),
            )
            .child(
                Input::new(input)
                    .small()
                    .aria_label(label)
                    .when(label == "Refresh interval", |input| {
                        input.suffix(
                            div()
                                .text_color(cx.theme().muted_foreground)
                                .child("seconds"),
                        )
                    })
                    .disabled(env_name.is_some()),
            )
            .when(label == "Refresh interval", |field| {
                field.child(
                    div()
                        .text_size(px(12.))
                        .text_color(cx.theme().muted_foreground)
                        .child("Minimum 30 seconds. Default 300 (5 minutes)."),
                )
            })
            .when(self.error_field == Some(label), |field| {
                field.child(
                    div()
                        .text_size(px(12.))
                        .text_color(cx.theme().danger)
                        .child(self.error.clone().unwrap_or_default()),
                )
            })
    }
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let issue_env = self
            .env
            .issue_link
            .as_ref()
            .map(|_| "PRMARMOT_ISSUE_PATTERN + PRMARMOT_ISSUE_URL_TEMPLATE");
        v_flex()
            .max_h((window.viewport_size().height - px(190.)).min(px(500.)))
            .text_size(px(13.))
            .child(
                v_flex()
                    .id("settings-scroll")
                    .min_h_0()
                    .gap_3()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .pr_1()
                    .child(
                        h_flex().gap_4().pb_3()
                            .child(img("branding/mascot.png").w(px(58.)).h(px(64.)).flex_shrink_0())
                            .child(v_flex().gap_1()
                                .child(div().text_size(px(20.)).font_weight(FontWeight::SEMIBOLD).child("PR Marmot"))
                                .child(div().text_color(cx.theme().muted_foreground)
                                    .child(format!("Version {}", env!("CARGO_PKG_VERSION"))))),
                    )
                    .child(self.render_account(cx))
                    .child(
                        self.field(
                            "Reviewer suggestions",
                            &self.reviewers,
                            self.env
                                .reviewers
                                .as_ref()
                                .map(|_| "PRMARMOT_DEFAULT_REVIEWERS"),
                            cx,
                        ),
                    )
                    .child(div().text_size(px(12.)).text_color(cx.theme().muted_foreground)
                        .child("Comma-separated usernames, a hint only — no assignments or CODEOWNERS. Used where no [repo_reviewers] entry in config.toml matches the owner or repository. Leave empty for generic hints."))
                    .child(self.field(
                        "Refresh interval",
                        &self.refresh,
                        self.env.refresh.map(|_| "PRMARMOT_REFRESH_SECS"),
                        cx,
                    ))
                    .child(
                        v_flex()
                            .gap_1()
                            .child(h_flex().justify_between().child("Theme").when_some(
                                self.env.theme.as_ref(),
                                |row, _| {
                                    row.child(
                                        div()
                                            .text_size(px(11.))
                                            .text_color(cx.theme().muted_foreground)
                                            .child("Controlled by PRMARMOT_THEME"),
                                    )
                                },
                            ))
                            .child(
                                ButtonGroup::new("settings-theme")
                                    .small()
                                    .compact()
                                    .disabled(self.env.theme.is_some())
                                    .child(
                                        Button::new("settings-theme-system")
                                            .label("System")
                                            .selected(self.theme == ThemePref::System),
                                    )
                                    .child(
                                        Button::new("settings-theme-light")
                                            .label("Light")
                                            .selected(self.theme == ThemePref::Light),
                                    )
                                    .child(
                                        Button::new("settings-theme-dark")
                                            .label("Dark")
                                            .selected(self.theme == ThemePref::Dark),
                                    )
                                    .on_click(cx.listener(|this, selected: &Vec<usize>, _, cx| {
                                        this.theme = match selected.first() {
                                            Some(1) => ThemePref::Light,
                                            Some(2) => ThemePref::Dark,
                                            _ => ThemePref::System,
                                        };
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(self.render_section_order(cx))
                    .child(
                        h_flex()
                            .justify_between()
                            .child("Desktop notifications")
                            .child(
                                Button::new("settings-notifications")
                                    .small()
                                    .selected(self.notifications)
                                    .label(if self.notifications { "On" } else { "Off" })
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.notifications = !this.notifications;
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(
                        h_flex()
                            .justify_between()
                            .child("Notification sound")
                            .child(
                                Button::new("settings-notification-sound")
                                    .small()
                                    .disabled(!self.notifications)
                                    .selected(self.notification_sound)
                                    .label(if self.notification_sound { "On" } else { "Off" })
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.notification_sound = !this.notification_sound;
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(
                        h_flex()
                            .justify_between()
                            .child("Dock attention badge")
                            .child(
                                Button::new("settings-dock-badge")
                                    .small()
                                    .selected(self.dock_badge)
                                    .label(if self.dock_badge { "On" } else { "Off" })
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.dock_badge = !this.dock_badge;
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(
                        h_flex()
                            .justify_between()
                            .child("Check for updates automatically")
                            .child(
                                Button::new("settings-automatic-update-checks")
                                    .small()
                                    .selected(self.automatic_update_checks)
                                    .label(if self.automatic_update_checks { "On" } else { "Off" })
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.automatic_update_checks =
                                            !this.automatic_update_checks;
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(
                        h_flex().child(Button::new("settings-advanced")
                            .small()
                            .ghost()
                            .icon(if self.advanced {
                                IconName::ChevronDown
                            } else {
                                IconName::ChevronRight
                            })
                            .label("Advanced")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.advanced = !this.advanced;
                                cx.notify();
                            }))),
                    )
                    .when(self.advanced, |form| {
                        form.child(
                            h_flex()
                                .justify_between()
                                .child("Notify for every PR entering Needs action")
                                .child(
                                    Button::new("settings-notify-all")
                                        .small()
                                        .selected(self.notify_all_needs_action)
                                        .label(if self.notify_all_needs_action {
                                            "On"
                                        } else {
                                            "Off"
                                        })
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.notify_all_needs_action =
                                                !this.notify_all_needs_action;
                                            cx.notify();
                                        })),
                                ),
                        )
                                .child(div().text_size(px(12.)).text_color(cx.theme().muted_foreground)
                                    .child("Off by default. Watches remain independent."))
                                .child(self.field(
                                    "Issue ID regular expression",
                                    &self.issue_pattern,
                                    issue_env,
                                    cx,
                                ))
                                .child(self.field(
                                    "Issue URL template",
                                    &self.issue_url,
                                    issue_env,
                                    cx,
                                ))
                                .child(div().text_size(px(12.)).text_color(cx.theme().muted_foreground)
                                    .child("Use {id} for the matched issue ID. Clear both fields to disable issue links."))
                                .child(
                                    v_flex()
                                        .gap_1()
                                        .child("Config file")
                                        .child(
                                            div()
                                                .text_size(px(11.))
                                                .text_color(cx.theme().muted_foreground)
                                                .child(self.path.clone()),
                                        )
                                        .child(
                                            h_flex().child(Button::new("settings-copy-path")
                                                .small()
                                                .label(if self.path_copied { "Copied" } else { "Copy path" })
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    cx.write_to_clipboard(
                                                        ClipboardItem::new_string(this.path.clone()),
                                                    );
                                                    this.path_copied = true;
                                                    cx.notify();
                                                }))),
                                        ),
                                )
                    }),
            )
            .when_some(self.error.clone().filter(|_| self.error_field.is_none()), |form, error| {
                form.child(div().mt_3().text_color(cx.theme().danger).child(error))
            })
            .child(
                h_flex()
                    .flex_shrink_0()
                    .justify_end()
                    .gap_2()
                    .mt_4()
                    .child(
                        Button::new("settings-cancel")
                            .label("Cancel")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("settings-save")
                            .primary()
                            .label("Save")
                            .on_click(cx.listener(|this, _, window, cx| this.save(window, cx))),
                    ),
            )
    }
}

fn resolve_theme(value: Option<&str>) -> ThemePref {
    match value {
        Some("light") => ThemePref::Light,
        Some("dark") => ThemePref::Dark,
        _ => ThemePref::System,
    }
}

fn validate_refresh(value: &str) -> Result<u64, String> {
    value
        .trim()
        .parse::<u32>()
        .map_err(|_| "Refresh interval must be a whole number of seconds".to_owned())
        .and_then(|secs| {
            (secs >= 30)
                .then_some(u64::from(secs))
                .ok_or_else(|| "Refresh interval must be at least 30 seconds".to_owned())
        })
}

fn validate_reviewers(value: &str) -> Result<Vec<String>, String> {
    let mut reviewers = Vec::new();
    for raw in value.split(',') {
        let login = raw.trim();
        if login.is_empty() {
            continue;
        }
        let valid = login.len() <= 39
            && !login.starts_with('-')
            && !login.ends_with('-')
            && login.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
        if !valid {
            return Err(format!("Invalid GitHub username: {login}"));
        }
        if !reviewers
            .iter()
            .any(|saved: &String| saved.eq_ignore_ascii_case(login))
        {
            reviewers.push(login.to_owned());
        }
    }
    Ok(reviewers)
}

#[derive(Debug)]
struct FieldError {
    field: &'static str,
    message: String,
}

fn validate_issue_link(pattern: &str, url: &str) -> Result<Option<(String, String)>, FieldError> {
    if pattern.is_empty() && url.is_empty() {
        return Ok(None);
    }
    if pattern.is_empty() || url.is_empty() {
        return Err(FieldError {
            field: if pattern.is_empty() {
                "Issue ID regular expression"
            } else {
                "Issue URL template"
            },
            message: "Fill both issue fields, or clear both to disable issue links".into(),
        });
    }
    if !(url.starts_with("https://") || url.starts_with("http://")) || !url.contains("{id}") {
        return Err(FieldError {
            field: "Issue URL template",
            message: "Issue URL must use http:// or https:// and contain {id}".into(),
        });
    }
    IssueLinkRule::new(pattern, url).map_err(|e| FieldError {
        field: "Issue ID regular expression",
        message: format!("Invalid issue pattern: {e}"),
    })?;
    Ok(Some((pattern.to_owned(), url.to_owned())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_account_block_names_the_live_sign_in_and_an_unused_token() {
        let stored = StoredAccount {
            who: "octo".into(),
            how: "signed in with GitHub",
            place: "the macOS keychain".into(),
        };
        let gh = Some((Connection::GhCli, "octo"));
        assert_eq!(
            account_detail(gh, None, false, "github.com"),
            "Using your GitHub CLI login as octo."
        );
        let both = account_detail(gh, Some(&stored), false, "github.com");
        assert!(
            both.starts_with("Using your GitHub CLI login as octo."),
            "{both}"
        );
        assert!(both.contains("is unused"), "{both}");
        assert!(both.contains("Disconnect removes it"), "{both}");
        assert_eq!(
            account_detail(
                Some((Connection::Device, "octo")),
                Some(&stored),
                false,
                "github.com"
            ),
            "Using a stored token as octo (signed in with GitHub). Token in the macOS keychain."
        );
        assert_eq!(
            account_detail(None, None, false, "ghe.acme.test"),
            "Not signed in to ghe.acme.test."
        );
        let token = Some((Connection::Token, "octo"));
        assert!(account_detail(token, None, true, "github.com").contains("PRMARMOT_TOKEN"));
        assert_eq!(
            account_detail(token, Some(&stored), true, "github.com"),
            "Using the token from PRMARMOT_TOKEN as octo."
        );
        assert!(account_detail(token, None, false, "github.com").contains("sign in again"));
    }

    #[test]
    fn reviewer_validation_trims_deduplicates_and_checks_logins() {
        assert_eq!(
            validate_reviewers(" Alice, bob, ALICE ").unwrap(),
            vec!["Alice", "bob"]
        );
        assert!(validate_reviewers("bad_name").is_err());
        assert!(validate_reviewers("-bad").is_err());
    }

    #[test]
    fn refresh_requires_numeric_floor() {
        assert_eq!(validate_refresh(" 30 ").unwrap(), 30);
        assert!(validate_refresh("29").is_err());
        assert!(validate_refresh("soon").is_err());
    }

    #[test]
    fn issue_link_is_paired_and_validated() {
        assert_eq!(validate_issue_link("", "").unwrap(), None);
        for (pattern, url, field) in [
            ("", "https://x.test/{id}", "Issue ID regular expression"),
            ("PROJ-[0-9]+", "", "Issue URL template"),
            ("(", "https://x.test/{id}", "Issue ID regular expression"),
            ("PROJ-[0-9]+", "file://x/{id}", "Issue URL template"),
            ("PROJ-[0-9]+", "https://x.test/no-id", "Issue URL template"),
        ] {
            assert_eq!(validate_issue_link(pattern, url).unwrap_err().field, field);
        }
        assert_eq!(
            validate_issue_link("PROJ-[0-9]+", "https://x.test/{id}").unwrap(),
            Some(("PROJ-[0-9]+".into(), "https://x.test/{id}".into()))
        );
    }
}
