//! Signing in to GitHub from inside the app, for people who do not have the
//! `gh` CLI.
//!
//! Three paths, in the order the design doc puts them: the OAuth **device
//! flow** (a one-time code typed at github.com — no client secret, no server
//! of ours), a pasted **personal access token**, and an **Enterprise Server
//! host** field that switches both.
//!
//! The protocol is `prmarmot_core::github::device_flow`; the storage is
//! `prmarmot_local::auth`. This file only owns the screen and the poll timer.

use std::sync::Arc;
use std::time::Duration;

use gpui::prelude::FluentBuilder;
use gpui::{
    div, px, App, AppContext, ClipboardItem, Context, Entity, EventEmitter, FontWeight,
    IntoElement, ParentElement, Render, Styled, Window,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputState};
use gpui_component::{h_flex, v_flex, ActiveTheme, Sizable};
use prmarmot_core::github::device_flow::{DeviceCode, DeviceFlow, DevicePoll};
use prmarmot_core::github::{normalize_host, viewer_login, AuthTransport, GhError};
use prmarmot_local::auth::{token_store, StoredAuth};
use prmarmot_local::config::AuthSettings;
use prmarmot_local::session;

/// Emitted once a token is stored, so the shell can re-run its setup check.
#[derive(Clone, Copy, Debug)]
pub struct SignedIn;

impl EventEmitter<SignedIn> for OnboardingView {}

/// Where the sign-in is.
enum Step {
    /// The three choices.
    Choose,
    /// A request is in flight (starting a flow, verifying a token).
    Working(&'static str),
    /// GitHub issued a code; the user is finishing at github.com.
    Waiting(DeviceCode),
    /// Paste a personal access token.
    Token,
    /// Type a GitHub Enterprise Server hostname.
    Host,
}

pub struct OnboardingView {
    settings: AuthSettings,
    step: Step,
    token_input: Entity<InputState>,
    host_input: Entity<InputState>,
    /// The last thing that went wrong, in the user's words.
    message: Option<String>,
    /// Bumped whenever the user starts over; a poll from an older attempt is
    /// ignored instead of writing a token nobody is waiting for.
    attempt: u64,
}

impl OnboardingView {
    pub fn new(settings: AuthSettings, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let host = settings.host.clone();
        Self {
            settings,
            step: Step::Choose,
            token_input: cx.new(|cx| {
                InputState::new(window, cx)
                    .masked(true)
                    .placeholder("ghp_… or github_pat_…")
            }),
            host_input: cx.new(|cx| {
                InputState::new(window, cx)
                    .default_value(host)
                    .placeholder("github.com")
            }),
            message: None,
            attempt: 0,
        }
    }

    fn user_agent(&self) -> String {
        session::user_agent("prmarmot", env!("CARGO_PKG_VERSION"))
    }

    fn transport(&self) -> Arc<dyn AuthTransport> {
        session::auth_transport(&self.settings.host, &self.user_agent())
    }

    fn fail(&mut self, error: &GhError, cx: &mut Context<Self>) {
        self.step = Step::Choose;
        self.message = Some(error.to_string());
        cx.notify();
    }

    /// Verify the token against GitHub, remember the login, store it.
    fn store(&mut self, stored: StoredAuth, attempt: u64, cx: &mut Context<Self>) {
        let host = self.settings.host.clone();
        let agent = self.user_agent();
        let store_kind = self.settings.store;
        self.step = Step::Working("Checking the token…");
        self.message = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let probe = session::probe_transport(&host, &stored.token.access_token, &agent);
                    let login = viewer_login(&probe)?;
                    let stored = stored.with_login(login);
                    token_store(store_kind)
                        .save(&stored)
                        .map_err(GhError::Network)?;
                    Ok::<(), GhError>(())
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.attempt != attempt {
                    return;
                }
                match result {
                    Ok(()) => {
                        this.step = Step::Choose;
                        this.message = None;
                        cx.emit(SignedIn);
                        cx.notify();
                    }
                    Err(error) => this.fail(&error, cx),
                }
            });
        })
        .detach();
    }

    fn start_device_flow(&mut self, cx: &mut Context<Self>) {
        if self.settings.client_id_is_placeholder() {
            self.message = Some(format!(
                "No GitHub client ID is configured for {}. Set [auth] client_id in the config \
                 file (or PRMARMOT_CLIENT_ID), or use a token instead.",
                self.settings.host
            ));
            cx.notify();
            return;
        }
        self.attempt += 1;
        let attempt = self.attempt;
        self.step = Step::Working("Asking GitHub for a code…");
        self.message = None;
        cx.notify();

        let flow = DeviceFlow::new(&self.settings.host, &self.settings.client_id);
        let transport = self.transport();
        cx.spawn(async move |this, cx| {
            let started = {
                let flow = flow.clone();
                let transport = transport.clone();
                cx.background_executor()
                    .spawn(async move { flow.start(transport.as_ref()) })
                    .await
            };
            let code = match started {
                Ok(code) => code,
                Err(error) => {
                    let _ = this.update(cx, |this, cx| {
                        if this.attempt == attempt {
                            this.fail(&error, cx);
                        }
                    });
                    return;
                }
            };
            let _ = this.update(cx, |this, cx| {
                if this.attempt != attempt {
                    return;
                }
                this.step = Step::Waiting(code.clone());
                cx.notify();
            });

            // Poll until GitHub answers, the code expires, or the user starts
            // over. Sleeping lives here; core stays free of timers.
            let mut interval = Duration::from_secs(code.interval_secs);
            let mut waited = Duration::ZERO;
            let limit = Duration::from_secs(code.expires_in_secs);
            while waited < limit {
                cx.background_executor().timer(interval).await;
                waited += interval;
                let polled = {
                    let flow = flow.clone();
                    let transport = transport.clone();
                    let device_code = code.device_code.clone();
                    cx.background_executor()
                        .spawn(async move {
                            flow.poll(
                                transport.as_ref(),
                                &device_code,
                                chrono::Utc::now().timestamp(),
                            )
                        })
                        .await
                };
                let keep_going = this.update(cx, |this, cx| {
                    if this.attempt != attempt {
                        return false;
                    }
                    match polled {
                        Ok(DevicePoll::Pending) => true,
                        Ok(DevicePoll::SlowDown { interval_secs }) => {
                            interval = Duration::from_secs(interval_secs);
                            true
                        }
                        Ok(DevicePoll::Token(token)) => {
                            let stored = StoredAuth::device(
                                &this.settings.host,
                                &this.settings.client_id,
                                *token,
                            );
                            this.store(stored, attempt, cx);
                            false
                        }
                        Ok(DevicePoll::Expired) => {
                            this.step = Step::Choose;
                            this.message =
                                Some("The code expired. Start again to get a new one.".into());
                            cx.notify();
                            false
                        }
                        Ok(DevicePoll::Denied) => {
                            this.step = Step::Choose;
                            this.message = Some("The request was declined at GitHub.".into());
                            cx.notify();
                            false
                        }
                        Err(error) => {
                            this.fail(&error, cx);
                            false
                        }
                    }
                });
                if !matches!(keep_going, Ok(true)) {
                    return;
                }
            }
            let _ = this.update(cx, |this, cx| {
                if this.attempt != attempt {
                    return;
                }
                this.step = Step::Choose;
                this.message = Some("The code expired. Start again to get a new one.".into());
                cx.notify();
            });
        })
        .detach();
    }

    fn submit_token(&mut self, cx: &mut Context<Self>) {
        let token = self.token_input.read(cx).value().trim().to_owned();
        if token.is_empty() {
            self.message = Some("Paste a personal access token first.".into());
            cx.notify();
            return;
        }
        self.attempt += 1;
        let attempt = self.attempt;
        let stored = StoredAuth::pat(&self.settings.host, &token);
        self.store(stored, attempt, cx);
    }

    fn submit_host(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let host = normalize_host(self.host_input.read(cx).value().as_ref());
        self.settings.host = host.clone();
        self.host_input
            .update(cx, |input, cx| input.set_value(host, window, cx));
        self.attempt += 1;
        self.step = Step::Choose;
        self.message = Some(
            "An Enterprise Server needs its own app registration, so set [auth] client_id for \
             this host, or sign in with a token."
                .into(),
        );
        cx.notify();
    }

    fn heading(&self, text: &str) -> impl IntoElement {
        div()
            .font_weight(FontWeight::SEMIBOLD)
            .text_size(px(18.))
            .child(text.to_owned())
    }

    fn note(&self, text: String, cx: &App) -> impl IntoElement {
        div()
            .max_w(px(560.))
            .text_color(cx.theme().muted_foreground)
            .child(text)
    }
}

impl Render for OnboardingView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let body = v_flex().gap_3().items_center().max_w(px(600.));
        let body = match &self.step {
            Step::Working(what) => body
                .child(self.heading("Signing in"))
                .child(self.note((*what).to_owned(), cx)),
            Step::Waiting(code) => {
                let user_code = code.user_code.clone();
                let uri = code.verification_uri.clone();
                body.child(self.heading("Enter this code at GitHub"))
                    .child(
                        div()
                            .px_4()
                            .py_2()
                            .rounded(px(6.))
                            .bg(theme.muted)
                            .font_family("monospace")
                            .text_size(px(24.))
                            .child(user_code.clone()),
                    )
                    .child(self.note(
                        format!(
                            "Open {uri}, sign in to GitHub, and type the code. \
                             PR Marmot is waiting and will continue on its own."
                        ),
                        cx,
                    ))
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("copy-device-code")
                                    .small()
                                    .label("Copy code")
                                    .on_click(cx.listener(move |_, _, _, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(
                                            user_code.clone(),
                                        ));
                                    })),
                            )
                            .child(
                                Button::new("open-device-url")
                                    .small()
                                    .primary()
                                    .label("Open GitHub")
                                    .on_click(cx.listener(move |_, _, _, cx| {
                                        cx.open_url(&uri);
                                    })),
                            )
                            .child(
                                Button::new("cancel-device-flow")
                                    .small()
                                    .label("Cancel")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.attempt += 1;
                                        this.step = Step::Choose;
                                        this.message = None;
                                        cx.notify();
                                    })),
                            ),
                    )
            }
            Step::Token => body
                .child(self.heading("Use a personal access token"))
                .child(self.note(
                    "A fine-grained token needs Pull requests: read and Metadata: read, and \
                     covers one owner; a classic token with `repo` covers several organizations. \
                     The token is stored on this Mac only."
                        .to_owned(),
                    cx,
                ))
                .child(div().w(px(420.)).child(Input::new(&self.token_input).small()))
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("submit-token")
                                .small()
                                .primary()
                                .label("Sign in")
                                .on_click(cx.listener(|this, _, _, cx| this.submit_token(cx))),
                        )
                        .child(
                            Button::new("token-back")
                                .small()
                                .label("Back")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.step = Step::Choose;
                                    this.message = None;
                                    cx.notify();
                                })),
                        ),
                ),
            Step::Host => body
                .child(self.heading("GitHub Enterprise Server"))
                .child(self.note(
                    "The hostname of your instance, without https://. Its own app registration \
                     supplies the client ID; a token works without one."
                        .to_owned(),
                    cx,
                ))
                .child(div().w(px(420.)).child(Input::new(&self.host_input).small()))
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("submit-host")
                                .small()
                                .primary()
                                .label("Use this host")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.submit_host(window, cx)
                                })),
                        )
                        .child(
                            Button::new("host-back")
                                .small()
                                .label("Back")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.step = Step::Choose;
                                    this.message = None;
                                    cx.notify();
                                })),
                        ),
                ),
            Step::Choose => body
                .child(self.heading("Sign in to GitHub"))
                .child(self.note(
                    format!(
                        "PR Marmot reads {} directly. Your token stays on this Mac and is never \
                         sent anywhere else.",
                        self.settings.host
                    ),
                    cx,
                ))
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("sign-in-device")
                                .primary()
                                .label("Sign in with GitHub")
                                .on_click(cx.listener(|this, _, _, cx| this.start_device_flow(cx))),
                        )
                        .child(
                            Button::new("sign-in-token")
                                .label("Use a token")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.step = Step::Token;
                                    this.message = None;
                                    cx.notify();
                                })),
                        )
                        .child(
                            Button::new("sign-in-host")
                                .label("Enterprise host…")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.step = Step::Host;
                                    this.message = None;
                                    cx.notify();
                                })),
                        ),
                ),
        };
        body.when_some(self.message.clone(), |body, message| {
            body.child(
                div()
                    .max_w(px(560.))
                    .text_size(px(12.))
                    .text_color(cx.theme().danger)
                    .child(message),
            )
        })
    }
}
