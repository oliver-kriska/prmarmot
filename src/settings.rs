//! Compact editable settings form hosted by the application's dialog shell.

use gpui::prelude::FluentBuilder;
use gpui::{
    div, img, px, App, AppContext, ClipboardItem, Context, Div, Entity, EventEmitter, FocusHandle,
    Focusable, FontWeight, InteractiveElement, IntoElement, ParentElement, Render, ScrollHandle,
    StatefulInteractiveElement, Styled, Window,
};
use gpui_component::button::{Button, ButtonGroup, ButtonVariants};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{h_flex, v_flex, ActiveTheme, Disableable, IconName, Sizable};
use prmarmot_core::board::{issue_link_problem, IssueLinkField};
use prmarmot_core::layout::{
    section_name, section_views, SectionKind, SectionOrder, ORDERABLE_SECTIONS,
};

use prmarmot_local::auth::{token_store, TokenKind};
use prmarmot_local::config::{AuthSettings, EnvOverrides};
use prmarmot_local::session::Connection;

use crate::config::{self, SettingsUpdate};
use crate::design::type_size;
use crate::theme::ThemePref;
use gpui_component::select::{SearchableVec, Select, SelectEvent, SelectState};
use gpui_component::IndexPath;
use prmarmot_local::config::{Colors, DetailsPosition, Skin};
use std::rc::Rc;

/// A path with the home directory written as `~`.
/// A segment or an On/Off toggle: `skin::segment`, with its on state said by
/// the toggled role too.
fn segment(button: Button, selected: bool, cx: &App) -> Button {
    crate::skin::segment(button.toggled(selected), selected, cx)
}

/// A group's heading: a step bolder than the labels under it, with a little
/// room above (critique S1).
fn heading(text: &'static str) -> Div {
    div().pt_2().font_weight(FontWeight::SEMIBOLD).child(text)
}

fn home_relative(path: &std::path::Path) -> String {
    let shown = path.display().to_string();
    match std::env::var_os("HOME").map(std::path::PathBuf::from) {
        Some(home) if path.starts_with(&home) => {
            format!("~/{}", path.strip_prefix(&home).unwrap_or(path).display())
        }
        _ => shown,
    }
}

/// One entry of the Colours picker.
struct ColorChoice {
    label: String,
    colors: Colors,
}

/// The built-in sets, then each theme in the themes folder once by name
/// (a light and a dark theme of one name are one choice).
fn color_choices(look: &crate::skin::Look) -> Vec<ColorChoice> {
    let mut choices: Vec<ColorChoice> = Colors::BUILT_IN
        .iter()
        .map(|colors| ColorChoice {
            label: colors.label().to_owned(),
            colors: colors.clone(),
        })
        .collect();
    for theme in &look.listing.themes {
        // One choice per label: a theme file named like a built-in, or like
        // another file but for case, would list twice and the picker's
        // Confirm resolves a label to its first match.
        if choices
            .iter()
            .any(|c| c.label.eq_ignore_ascii_case(&theme.name))
        {
            continue;
        }
        choices.push(ColorChoice {
            label: theme.name.clone(),
            colors: Colors::Custom(theme.name.clone()),
        });
    }
    choices
}

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
pub enum SettingsEvent {
    LookChanged,
    Preview,
    Back,
    Cancel,
    Saved,
}

impl EventEmitter<SettingsEvent> for SettingsView {}

pub struct SettingsView {
    focus_handle: FocusHandle,
    reviewers: Entity<InputState>,
    refresh: Entity<InputState>,
    background_refresh: Entity<InputState>,
    thread_refresh: Entity<InputState>,
    cache_dir: Entity<InputState>,
    repositories_dir: Entity<InputState>,
    default_harness: Entity<InputState>,
    default_model: Entity<InputState>,
    default_prompt: Entity<InputState>,
    default_agent_permissions: Entity<InputState>,
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
    /// The sections hidden in every view, in the default order, and what the
    /// file holds: saved only when changed.
    hidden_sections: Vec<SectionKind>,
    saved_hidden_sections: Vec<SectionKind>,
    /// Where Details opens, and what the file holds: saved only when changed.
    details_position: DetailsPosition,
    saved_details_position: DetailsPosition,
    /// The colours and the skin being edited, and what the file holds.
    colors: Colors,
    saved_colors: Colors,
    skin: Skin,
    saved_skin: Skin,
    /// The colour choices in the picker's order: the built-in sets, then
    /// every theme in the themes folder by name.
    color_choices: Vec<ColorChoice>,
    colors_select: Entity<SelectState<SearchableVec<String>>>,
    /// The theme folder's listing when Settings opened: its problems and
    /// the chosen theme's contrast warning.
    look: Rc<crate::skin::Look>,
    /// The same draft can be shown in a compact, nonmodal board preview.
    pub previewing: bool,
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
    fn choose_repositories_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let selection = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose repositories folder".into()),
        });
        cx.spawn_in(window, async move |this, cx| match selection.await {
            Ok(Ok(Some(paths))) => {
                if let Some(path) = paths.first() {
                    let value = path.to_string_lossy().into_owned();
                    let _ = this.update_in(cx, |this, window, cx| {
                        this.repositories_dir
                            .update(cx, |input, cx| input.set_value(value, window, cx));
                    });
                }
            }
            Ok(Ok(None)) => {}
            _ => {
                let _ = this.update(cx, |this, cx| {
                    this.show_error("Could not open folder picker".into(), cx)
                });
            }
        })
        .detach();
    }

    pub fn new(
        live: Option<(Connection, String)>,
        theme: ThemePref,
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
            .refresh_secs
            .or(file.refresh_secs)
            .unwrap_or(prmarmot_core::github::rate_limit::DEFAULT_REFRESH_SECS)
            .max(prmarmot_core::github::rate_limit::MIN_REFRESH_SECS)
            .to_string();
        let auth = {
            let mut warnings = Vec::new();
            prmarmot_local::config::auth_settings(&file, None, None, &mut warnings)
        };
        let account = stored_account(&auth);
        let section_order = prmarmot_local::config::section_order(&file);
        let hidden_sections = prmarmot_local::config::hidden_sections(&file);
        let details_position = prmarmot_local::config::details_position(&file);
        let look = Rc::new(crate::skin::Look::from_file(&file));
        let colors = look.colors.clone();
        let skin = look.skin;
        let color_choices = color_choices(&look);
        let selected = color_choices
            .iter()
            .position(|choice| choice.colors == colors)
            .map(IndexPath::new);
        let colors_select = cx.new(|cx| {
            SelectState::new(
                SearchableVec::new(
                    color_choices
                        .iter()
                        .map(|choice| choice.label.clone())
                        .collect::<Vec<_>>(),
                ),
                selected,
                window,
                cx,
            )
        });
        let issue = env.issue_link.clone().or_else(|| {
            file.issue_link
                .map(|rule| (rule.pattern, rule.url_template))
        });
        let (pattern, url) = issue.unwrap_or_default();

        let this = Self {
            focus_handle: cx.focus_handle(),
            reviewers: cx.new(|cx| InputState::new(window, cx).default_value(reviewers)),
            refresh: cx.new(|cx| InputState::new(window, cx).default_value(refresh)),
            background_refresh: cx.new(|cx| {
                InputState::new(window, cx)
                    .default_value(file.background_refresh_secs.unwrap_or(900).to_string())
            }),
            thread_refresh: cx.new(|cx| {
                InputState::new(window, cx)
                    .default_value(file.thread_refresh_secs.unwrap_or(600).to_string())
            }),
            default_harness: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("codex, claude, or copilotcli")
                    .default_value(file.default_harness.clone().unwrap_or_default())
            }),
            default_agent_permissions: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("Blank uses VS Code defaults; Codex: full-access")
                    .default_value(file.default_agent_permissions.clone().unwrap_or_default())
            }),
            default_prompt: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("Optional task prompt; e.g. /code-review")
                    .default_value(file.default_prompt.clone().unwrap_or_default())
            }),
            default_model: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("Model ID or name from VS Code Agents")
                    .default_value(file.default_model.clone().unwrap_or_default())
            }),
            repositories_dir: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("Absolute folder containing your repositories")
                    .default_value(file.repositories_dir.clone().unwrap_or_default())
            }),
            cache_dir: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("Blank for the default location")
                    .default_value(file.cache_dir.clone().unwrap_or_default())
            }),
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
            saved_hidden_sections: hidden_sections.clone(),
            hidden_sections,
            details_position,
            saved_details_position: details_position,
            saved_colors: colors.clone(),
            colors,
            skin,
            saved_skin: skin,
            color_choices,
            colors_select,
            look,
            previewing: false,
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
        cx.subscribe_in(
            &this.colors_select,
            window,
            |this: &mut Self, _, event: &SelectEvent<SearchableVec<String>>, _, cx| {
                let SelectEvent::Confirm(Some(label)) = event else {
                    return;
                };
                if let Some(choice) = this.color_choices.iter().find(|c| c.label == *label) {
                    this.colors = choice.colors.clone();
                    cx.emit(SettingsEvent::LookChanged);
                    cx.notify();
                }
            },
        )
        .detach();
        for (input, field) in [
            (&this.reviewers, "Reviewer suggestions"),
            (&this.refresh, "Refresh interval"),
            (&this.background_refresh, "Background refresh interval"),
            (&this.thread_refresh, "Thread refresh interval"),
            (&this.cache_dir, "Cache location"),
            (&this.repositories_dir, "Repositories folder"),
            (&this.default_harness, "Default agent harness"),
            (&this.default_model, "Default agent model"),
            (&this.default_prompt, "Default agent prompt"),
            (&this.default_agent_permissions, "Default agent permissions"),
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
        let refresh = if let Some(value) = self.env.refresh_secs {
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

        let background_refresh =
            match validate_refresh(self.background_refresh.read(cx).value().as_str()) {
                Ok(value) => value,
                Err(error) => {
                    return self.show_invalid("Background refresh interval", error, window, cx)
                }
            };
        let thread_refresh = match validate_refresh(self.thread_refresh.read(cx).value().as_str()) {
            Ok(value) => value,
            Err(error) => return self.show_invalid("Thread refresh interval", error, window, cx),
        };
        let cache_dir = self.cache_dir.read(cx).value().trim().to_owned();
        if !cache_dir.is_empty() && !std::path::Path::new(&cache_dir).is_absolute() {
            return self.show_invalid(
                "Cache location",
                "Use an absolute directory path, or leave blank for the default".into(),
                window,
                cx,
            );
        }
        let repositories_dir = self.repositories_dir.read(cx).value().trim().to_owned();
        if !repositories_dir.is_empty() && !std::path::Path::new(&repositories_dir).is_absolute() {
            return self.show_invalid(
                "Repositories folder",
                "Use an absolute directory path".into(),
                window,
                cx,
            );
        }
        let update = SettingsUpdate {
            repositories_dir: Some(repositories_dir),
            default_harness: Some(self.default_harness.read(cx).value().trim().to_owned()),
            default_model: Some(self.default_model.read(cx).value().trim().to_owned()),
            default_prompt: Some(self.default_prompt.read(cx).value().trim().to_owned()),
            default_agent_permissions: Some(
                self.default_agent_permissions
                    .read(cx)
                    .value()
                    .trim()
                    .to_owned(),
            ),
            background_refresh_secs: Some(background_refresh),
            thread_refresh_secs: Some(thread_refresh),
            cache_dir: Some(cache_dir),
            default_reviewers: self.env.reviewers.is_none().then_some(reviewers),
            refresh_secs: self.env.refresh_secs.is_none().then_some(refresh),
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
            hidden_sections: (self.hidden_sections != self.saved_hidden_sections)
                .then(|| self.hidden_sections.clone()),
            details_position: (self.details_position != self.saved_details_position)
                .then_some(self.details_position),
            colors: (self.colors != self.saved_colors).then(|| self.colors.clone()),
            skin: (self.skin != self.saved_skin).then_some(self.skin),
        };
        match config::save_settings(&update) {
            Ok(()) => {
                self.error = None;
                cx.emit(SettingsEvent::Saved);
            }
            Err(error) => self.show_error(error, cx),
        }
    }

    /// Reuse the parsed themes. Previewing never reads files or fetches PRs.
    pub fn preview(&self) -> (crate::skin::Look, ThemePref) {
        let mut look = (*self.look).clone();
        look.colors = self.colors.clone();
        look.skin = self.skin;
        (look, self.theme)
    }

    fn skin_choice(&self, index: usize, skin: Skin, cx: &Context<Self>) -> Button {
        let theme = cx.theme();
        let selected = self.skin == skin;
        // Both miniatures share the current palette; only geometry differs.
        let miniature = v_flex()
            .w_full()
            .h(px(66.))
            .bg(theme.title_bar)
            .border_1()
            .border_color(theme.border)
            .rounded(px(5.))
            .overflow_hidden()
            .child(
                h_flex().h(px(10.)).px_1().items_center().child(
                    div()
                        .w(px(25.))
                        .h(px(3.))
                        .rounded_sm()
                        .bg(theme.muted_foreground),
                ),
            )
            .child(
                v_flex()
                    .flex_1()
                    .when(skin == Skin::Glacier, |sheet| sheet.m_1().rounded(px(4.)))
                    .bg(theme.background)
                    .px_1()
                    .justify_evenly()
                    .children(
                        [0.75, 0.5, 0.85]
                            .into_iter()
                            .enumerate()
                            .map(|(ix, width)| {
                                h_flex()
                                    .gap_1()
                                    .items_center()
                                    .child(div().size(px(3.)).rounded_full().bg(if ix == 1 {
                                        theme.danger
                                    } else {
                                        theme.muted_foreground
                                    }))
                                    .child(
                                        div()
                                            .h(px(3.))
                                            .w(gpui::relative(width))
                                            .rounded_sm()
                                            .bg(theme.muted),
                                    )
                            }),
                    ),
            );
        segment(Button::new(("settings-skin", index)), selected, cx)
            .flex_1()
            .h(px(108.))
            .p_2()
            .accessibility_label(format!("{} skin", skin.label()))
            .child(
                v_flex().w_full().gap_2().child(miniature).child(
                    h_flex()
                        .justify_between()
                        .child(skin.label())
                        .child(div().w(px(14.)).child(if selected { "✓" } else { "" })),
                ),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.skin = skin;
                cx.emit(SettingsEvent::LookChanged);
                cx.notify();
            }))
    }

    /// What the themes folder could not provide: a theme file that was
    /// ignored, and the chosen theme's poor contrast (a warning, never a
    /// block). Three lines at most.
    fn look_warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        if let Colors::Custom(name) = &self.colors {
            for theme in self.look.listing.themes.iter().filter(|t| t.name == *name) {
                if let Some(warning) = &theme.contrast_warning {
                    warnings.push(warning.clone());
                }
            }
        }
        warnings.extend(self.look.listing.problems.iter().cloned());
        warnings.truncate(3);
        warnings
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
        if self.previewing {
            self.previewing = false;
            cx.emit(SettingsEvent::Back);
        }
        self.error_field = Some(field);
        // Indices are direct children of settings-scroll (the header, Account
        // and Look come first); Advanced fields are siblings so either input
        // can be revealed independently.
        let (input, index) = match field {
            "Reviewer suggestions" => (&self.reviewers, 3),
            "Refresh interval" => (&self.refresh, 5),
            "Background refresh interval" => (&self.background_refresh, 6),
            "Thread refresh interval" => (&self.thread_refresh, 7),
            "Cache location" => (&self.cache_dir, 9),
            "Repositories folder" => (&self.repositories_dir, 11),
            "Issue ID regular expression" => (&self.issue_pattern, 28),
            _ => (&self.issue_url, 29),
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
                h_flex()
                    .justify_between()
                    .items_end()
                    .child(heading("Account"))
                    .child(
                        div()
                            .text_size(type_size::CAPTION)
                            .text_color(muted)
                            .child(self.auth.host.clone()),
                    ),
            )
            .child(
                div()
                    .text_size(type_size::SMALL)
                    .text_color(muted)
                    .child(detail),
            )
            .when(self.account.is_some(), |block| {
                block.child(
                    crate::skin::quiet(Button::new("settings-disconnect"), cx)
                        .small()
                        .label("Disconnect")
                        .on_click(cx.listener(|this, _, _, cx| this.disconnect(cx))),
                )
            })
            .when_some(self.account_message.clone(), |block, message| {
                block.child(
                    div()
                        .text_size(type_size::SMALL)
                        .text_color(muted)
                        .child(message),
                )
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
            Err(error) => self.account_message = Some(error.to_string()),
        }
        cx.notify();
    }

    /// Section order: one list for every view, each section moved with its up
    /// and down buttons. The names and where each shows come from core, as
    /// the iPad's list does.
    /// Hide a shown section or show a hidden one, keeping the list in the
    /// default order as the file writes it.
    fn toggle_hidden_section(&mut self, kind: SectionKind) {
        if let Some(ix) = self.hidden_sections.iter().position(|k| *k == kind) {
            self.hidden_sections.remove(ix);
        } else {
            self.hidden_sections.push(kind);
            self.hidden_sections
                .sort_by_key(|kind| ORDERABLE_SECTIONS.iter().position(|k| k == kind));
        }
    }

    fn render_section_order(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let last = self.section_order.kinds().len().saturating_sub(1);
        v_flex()
            .gap_1()
            .child(
                h_flex()
                    .justify_between()
                    .items_end()
                    .child(heading("Section order"))
                    .child(
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
            .child(div().text_size(type_size::SMALL).text_color(muted).child(
                "The order sections come in, the same in every view; each view shows the ones it \
                 has. The eye hides a section in every view. Saved as section_order and \
                 hidden_sections in config.toml.",
            ))
            .children(
                self.section_order
                    .kinds()
                    .iter()
                    .copied()
                    .enumerate()
                    .map(|(ix, kind)| {
                        let name = section_name(kind);
                        let hidden = self.hidden_sections.contains(&kind);
                        h_flex()
                            .gap_2()
                            .child(
                                div()
                                    .w(px(14.))
                                    .text_color(muted)
                                    .child((ix + 1).to_string()),
                            )
                            .child(
                                div()
                                    .when(hidden, |name| name.text_color(muted).line_through())
                                    .child(name),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .text_size(type_size::CAPTION)
                                    .text_color(muted)
                                    .child(section_views(kind)),
                            )
                            .child(
                                Button::new(("settings-section-hide", ix))
                                    .small()
                                    .ghost()
                                    .icon(if hidden {
                                        IconName::EyeOff
                                    } else {
                                        IconName::Eye
                                    })
                                    .tooltip(if hidden {
                                        format!("Show {name}")
                                    } else {
                                        format!("Hide {name}")
                                    })
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.toggle_hidden_section(kind);
                                        cx.notify();
                                    })),
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
                                .text_size(type_size::CAPTION)
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
                        .text_size(type_size::SMALL)
                        .text_color(cx.theme().muted_foreground)
                        .child("Minimum 30 seconds. Default 300 (5 minutes)."),
                )
            })
            .when(self.error_field == Some(label), |field| {
                field.child(
                    div()
                        .text_size(type_size::SMALL)
                        .text_color(cx.theme().danger)
                        .child(self.error.clone().unwrap_or_default()),
                )
            })
    }
}

impl Focusable for SettingsView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let issue_env = self
            .env
            .issue_link
            .as_ref()
            .map(|_| "PRMARMOT_ISSUE_PATTERN + PRMARMOT_ISSUE_URL_TEMPLATE");
        let scrolls = self.scroll.max_offset().y > px(0.);
        v_flex()
            .track_focus(&self.focus_handle)
            .max_h((window.viewport_size().height - px(190.)).min(px(500.)))
            .text_size(type_size::BODY)
            .child(
                v_flex()
                    .id("settings-scroll")
                    .min_h_0()
                    .gap_3()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .pr_1()
                    .when(!self.previewing, |form| form.child(
                        h_flex().gap_4().pb_3()
                            .child(img("branding/mascot.png").w(px(58.)).h(px(64.)).flex_shrink_0())
                            .child(v_flex().gap_1()
                                .child(div().text_size(type_size::DISPLAY).font_weight(FontWeight::SEMIBOLD).child("PR Marmot"))
                                .child(div().text_color(cx.theme().muted_foreground)
                                    .child(format!("Version {}", env!("CARGO_PKG_VERSION"))))),
                    )
                    .child(self.render_account(cx)))
                    .child(
                        v_flex()
                            .gap_2()
                            .when(!self.previewing, |section| section.child(
                                h_flex().justify_between().child(heading("Look")).child(
                                    Button::new("settings-preview").small().ghost()
                                        .label("Preview on board")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.previewing = true;
                                            this.scroll.set_offset(gpui::point(px(0.), px(0.)));
                                            cx.emit(SettingsEvent::Preview);
                                            cx.notify();
                                        })),
                                ),
                            ))
                            .child(
                                h_flex()
                                    .justify_between()
                                    .items_center()
                                    .child(
                                        h_flex()
                                            .gap_2()
                                            .items_center()
                                            .child("Appearance")
                                            .when_some(self.env.theme.as_ref(), |row, _| {
                                                row.child(
                                                    div()
                                                        .text_size(type_size::CAPTION)
                                                        .text_color(cx.theme().muted_foreground)
                                                        .child("Controlled by PRMARMOT_THEME"),
                                                )
                                            }),
                                    )
                                    .child(
                                        ButtonGroup::new("settings-theme")
                                            .small()
                                            .compact()
                                            .disabled(self.env.theme.is_some())
                                            .child(segment(
                                                Button::new("settings-theme-system").label("System"),
                                                self.theme == ThemePref::System,
                                                cx,
                                            ))
                                            .child(segment(
                                                Button::new("settings-theme-light").label("Light"),
                                                self.theme == ThemePref::Light,
                                                cx,
                                            ))
                                            .child(segment(
                                                Button::new("settings-theme-dark").label("Dark"),
                                                self.theme == ThemePref::Dark,
                                                cx,
                                            ))
                                            .on_click(cx.listener(
                                                |this, selected: &Vec<usize>, _, cx| {
                                                    this.theme = match selected.first() {
                                                        Some(1) => ThemePref::Light,
                                                        Some(2) => ThemePref::Dark,
                                                        _ => ThemePref::System,
                                                    };
                                                    cx.emit(SettingsEvent::LookChanged);
                                                    cx.notify();
                                                },
                                            )),
                                    ),
                            )
                            .child(
                                v_flex()
                                    .gap_2()
                                    .child("Skin")
                                    .child(h_flex().gap_3().children(
                                        Skin::ALL.into_iter().enumerate().map(|(ix, skin)| {
                                            self.skin_choice(ix, skin, cx)
                                        }),
                                    ))
                                    // A caption reads as one paragraph under
                                    // its row, from the label column across
                                    // the dialog (a long themes path wrapped
                                    // to five right-aligned lines).
                                    .child(
                                        div()
                                            .text_size(type_size::CAPTION)
                                            .text_color(cx.theme().muted_foreground)
                                            .child("Classic fills the window with the table. Glacier sets it on a rounded sheet with a little air around it."),
                                    ),
                            )
                            .child(
                                v_flex()
                                    .gap_1()
                                    .child(
                                        h_flex()
                                            .justify_between()
                                            .items_center()
                                            .child("Colours")
                                            .child(
                                                div()
                                                    .w(px(220.))
                                                    .child(Select::new(&self.colors_select).small().accessibility_label("Colours")),
                                            ),
                                    )
                                    // The themes folder is a button, not a
                                    // path in prose (critique 2026-10-21:
                                    // a raw /var/folders path).
                                    .child(
                                        h_flex()
                                            .justify_between()
                                            .items_end()
                                            .gap_3()
                                            .child(
                                                div()
                                                    .min_w_0()
                                                    .text_size(type_size::CAPTION)
                                                    .text_color(cx.theme().muted_foreground)
                                                    .child("Classic, Studio and Mineral are built in. A theme file saved in the themes folder is listed here by its name."),
                                            )
                                            .child(
                                                Button::new("show-themes-folder")
                                                    .xsmall()
                                                    .ghost()
                                                    .flex_shrink_0()
                                                    .label("Show folder")
                                                    .tooltip(home_relative(&prmarmot_local::themes::themes_dir()))
                                                    .on_click(|_, _, cx| {
                                                        crate::open::open_folder(cx, &prmarmot_local::themes::themes_dir());
                                                    }),
                                            ),
                                    ),
                            )
                            .children(self.look_warnings().into_iter().map(|warning| {
                                div()
                                    .text_size(type_size::CAPTION)
                                    .text_color(cx.theme().warning)
                                    .child(warning)
                            })),
                    )
                    .when(!self.previewing, |form| form.child(
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
                    .child(div().text_size(type_size::SMALL).text_color(cx.theme().muted_foreground)
                        .child("Comma-separated usernames, a hint only — no assignments or CODEOWNERS. Used where no [repo_reviewers] entry in config.toml matches the owner or repository. Leave empty for generic hints."))
                    .child(self.field(
                        "Refresh interval",
                        &self.refresh,
                        self.env.refresh_secs.map(|_| "PRMARMOT_REFRESH_SECS"),
                        cx,
                    ))
                    .child(self.field("Background refresh interval", &self.background_refresh, None, cx))
                    .child(self.field("Thread refresh interval", &self.thread_refresh, None, cx))
                    .child(div().text_size(type_size::SMALL).child("Intervals are in seconds, minimum 30. Background polling is never faster than foreground polling. Cached thread replies update on the next board refresh after expiry. Manual board refresh also uses this cache."))
                    .child(self.field("Cache location", &self.cache_dir, None, cx))
                    .child(div().text_size(type_size::SMALL).child("Absolute directory for board and thread caches; blank uses the default. Changes apply to subsequent requests. Existing files are not moved. Watches and sign-in stay in their original locations."))
                    .child(v_flex()
                        .child(self.field("Repositories folder", &self.repositories_dir, None, cx))
                        .child(Button::new("choose-repositories-folder").small().label("Choose folder…")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.choose_repositories_folder(window, cx);
                            }))))
                    .child(div().text_size(type_size::SMALL).child("Open in VS Code searches this folder recursively. New checkouts use <repo>.worktrees/<branch-slug>."))
                    .child(self.field("Default agent harness", &self.default_harness, None, cx))
                    .child(self.field("Default agent model", &self.default_model, None, cx))
                    .child(self.field("Default agent prompt", &self.default_prompt, None, cx))
                    .child(self.field("Default agent permissions", &self.default_agent_permissions, None, cx))
                    .child(div().text_size(type_size::SMALL).child("Codex: default, auto-review, full-access. Claude: default, acceptEdits, plan, auto, bypassPermissions. Copilot: default, assisted, autoApprove. Blank keeps VS Code’s default. Unsupported values are reported before creating a chat."))
                    .child(div().text_size(type_size::SMALL).child("VS Code creates a PR session and manages its worktree from the parent repository using the selected harness/model. Blank prompt sends only a hidden setup message and waits for you; a custom prompt starts that task with the PR URL appended."))
                    .child(
                        v_flex()
                            .gap_1()
                            .child(heading("Details position"))
                            .child(
                                ButtonGroup::new("settings-details-position")
                                    .small()
                                    .compact()
                                    .children(DetailsPosition::ALL.into_iter().enumerate().map(|(ix, position)| {
                                        segment(
                                            Button::new(("settings-details", ix)).label(match position {
                                                DetailsPosition::Auto => "Automatic",
                                                DetailsPosition::Bottom => "Bottom",
                                                DetailsPosition::Right => "Right",
                                            }),
                                            self.details_position == position,
                                            cx,
                                        )
                                    }))
                                    .on_click(cx.listener(|this, selected: &Vec<usize>, _, cx| {
                                        if let Some(&ix) = selected.first() {
                                            this.details_position = DetailsPosition::ALL[ix];
                                            cx.notify();
                                        }
                                    })),
                            )
                            .child(
                                div()
                                    .text_size(type_size::CAPTION)
                                    .text_color(cx.theme().muted_foreground)
                                    .child("Automatic puts Details on the right when the window is wide enough to keep the full table beside it."),
                            ),
                    )
                    .child(self.render_section_order(cx))
                    .child(
                        h_flex()
                            .justify_between()
                            .child("Desktop notifications")
                            .child(
                                segment(Button::new("settings-notifications"), self.notifications, cx)
                                    .small()
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
                                segment(Button::new("settings-notification-sound"), self.notification_sound, cx)
                                    .small()
                                    .disabled(!self.notifications)
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
                                segment(Button::new("settings-dock-badge"), self.dock_badge, cx)
                                    .small()
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
                                segment(Button::new("settings-automatic-update-checks"), self.automatic_update_checks, cx)
                                    .small()
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
                                    segment(Button::new("settings-notify-all"), self.notify_all_needs_action, cx)
                                        .small()
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
                                .child(div().text_size(type_size::SMALL).text_color(cx.theme().muted_foreground)
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
                                .child(div().text_size(type_size::SMALL).text_color(cx.theme().muted_foreground)
                                    .child("Use {id} for the matched issue ID. The ID is read from the title, else from the branch name. Linear: [A-Z]+-[0-9]+ (or your team key, e.g. ENG-[0-9]+) and https://linear.app/<workspace>/issue/{id}; the Ticket column then links each PR to its Linear issue. Clear both fields to disable issue links."))
                                .child(
                                    v_flex()
                                        .gap_1()
                                        .child(heading("Config file"))
                                        .child(
                                            div()
                                                .text_size(type_size::CAPTION)
                                                .text_color(cx.theme().muted_foreground)
                                                .child(self.path.clone()),
                                        )
                                        .child(
                                            h_flex().child(crate::skin::quiet(Button::new("settings-copy-path"), cx)
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
                    })),
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
                    // When the form scrolls, a hairline says the content goes
                    // on under the buttons (critique S1: "Refresh interval"
                    // was cut with no cue). The scroll's range is the last
                    // frame's, which is fine: the form's height only changes
                    // with Advanced.
                    .when(scrolls, |row| {
                        row.border_t_1().border_color(cx.theme().border).pt_3().mt_3()
                    })
                    .when(self.previewing, |row| row.child(
                        Button::new("settings-back").ghost().label("Back to Settings")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.previewing = false;
                                cx.emit(SettingsEvent::Back);
                                cx.notify();
                            })),
                    ).child(div().flex_1()))
                    .child(
                        crate::skin::quiet(Button::new("settings-cancel"), cx)
                            .label("Cancel")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(SettingsEvent::Cancel))),
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

/// Core's check and core's words (`issue_link_problem`), the ones the iPad
/// shows too; this only says which field to mark.
fn validate_issue_link(pattern: &str, url: &str) -> Result<Option<(String, String)>, FieldError> {
    match issue_link_problem(pattern, url) {
        Some(problem) => Err(FieldError {
            field: match problem.field {
                IssueLinkField::Pattern => "Issue ID regular expression",
                IssueLinkField::Url => "Issue URL template",
            },
            message: problem.message,
        }),
        None if pattern.is_empty() => Ok(None),
        None => Ok(Some((pattern.to_owned(), url.to_owned()))),
    }
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
