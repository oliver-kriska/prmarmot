//! Compact editable settings form hosted by the application's dialog shell.

use gpui::prelude::FluentBuilder;
use gpui::{
    div, px, App, AppContext, ClipboardItem, Context, Entity, EventEmitter, InteractiveElement,
    IntoElement, ParentElement, Render, ScrollHandle, StatefulInteractiveElement, Styled, Window,
};
use gpui_component::button::{Button, ButtonGroup, ButtonVariants};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{
    h_flex, v_flex, ActiveTheme, Disableable, IconName, Selectable, Sizable, WindowExt,
};
use prmarmot_core::board::IssueLinkRule;

use crate::config::{self, SettingsUpdate};
use crate::theme::ThemePref;

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
    advanced: bool,
    error: Option<String>,
    error_field: Option<&'static str>,
    path_copied: bool,
    scroll: ScrollHandle,
    env: EnvOverrides,
    path: String,
}

impl SettingsView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
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
            advanced: false,
            error: None,
            error_field: None,
            path_copied: false,
            scroll: ScrollHandle::new(),
            env,
            path: config::config_path().display().to_string(),
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
        // Indices are direct children of settings-scroll; Advanced fields are
        // siblings so either input can be revealed independently.
        let (input, index) = match field {
            "Reviewer suggestions" => (&self.reviewers, 0),
            "Refresh interval" => (&self.refresh, 2),
            "Issue ID regular expression" => (&self.issue_pattern, 5),
            _ => (&self.issue_url, 6),
        };
        input.update(cx, |input, cx| input.focus(window, cx));
        self.scroll.scroll_to_top_of_item(index);
        self.show_error(error, cx);
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
                        .child("Comma-separated usernames. Applies to all repositories as a hint only — no assignments or CODEOWNERS. Leave empty for generic hints."))
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
