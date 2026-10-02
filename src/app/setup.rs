//! The screen shown until the board can load: checking the sign-in, or why
//! it could not.

use super::*;

impl RootView {
    pub(super) fn render_setup(&self, setup: SetupStatus, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        // Not signed in: the sign-in screen takes over, with the GitHub CLI
        // route offered underneath for people who already use it.
        if matches!(
            setup,
            SetupStatus::MissingGh | SetupStatus::NotAuthenticated
        ) {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_4()
                .px_4()
                .child(self.onboarding.clone())
                .child(
                    v_flex()
                        .gap_2()
                        .items_center()
                        .child(
                            div()
                                .max_w(px(560.))
                                .text_size(type_size::SMALL)
                                .text_color(theme.muted_foreground)
                                .child(
                                    "Already use the GitHub CLI? Run `gh auth login` in Terminal, \
                                     then choose Retry.",
                                ),
                        )
                        .child(
                            h_flex()
                                .gap_2()
                                .items_center()
                                .child(
                                    Button::new("copy-gh-auth")
                                        .small()
                                        .label("Copy `gh auth login`")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            cx.write_to_clipboard(ClipboardItem::new_string(
                                                "gh auth login".to_owned(),
                                            ));
                                            this.show_feedback("Command copied", cx);
                                        })),
                                )
                                .child(Button::new("retry-setup").small().label("Retry").on_click(
                                    cx.listener(|this, _, _, cx| {
                                        this.state.update(cx, |state, cx| state.validate_setup(cx));
                                    }),
                                )),
                        ),
                )
                .into_any_element();
        }
        let (title, detail) = match &setup {
            SetupStatus::Checking => (
                "Checking your GitHub sign-in…".to_owned(),
                "PR Marmot uses your GitHub CLI login when you have one, or a token you sign in \
                 with here."
                    .to_owned(),
            ),
            SetupStatus::MissingGh | SetupStatus::NotAuthenticated => {
                (String::new(), String::new())
            }
            SetupStatus::Network(_) => (
                "GitHub could not be reached".to_owned(),
                "Check your connection and your GitHub sign-in, then retry.".to_owned(),
            ),
            SetupStatus::Failed(message) => ("GitHub setup failed".to_owned(), message.clone()),
            SetupStatus::Ready => (String::new(), String::new()),
        };
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_3()
            .px_4()
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_size(type_size::TITLE)
                    .child(title),
            )
            .child(
                div()
                    .max_w(px(560.))
                    .text_color(theme.muted_foreground)
                    .child(detail),
            )
            .when(setup != SetupStatus::Checking, |view| {
                view.child(
                    Button::new("retry-setup")
                        .primary()
                        .label("Retry")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.state.update(cx, |state, cx| state.validate_setup(cx));
                        })),
                )
            })
            .into_any_element()
    }
}
