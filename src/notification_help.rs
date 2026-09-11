//! User-invoked guidance for enabling desktop notifications.
//!
//! This module deliberately does not own application state. The caller passes
//! a callback that starts a permission request or check through `Platform`.

use std::rc::Rc;

use gpui::{div, px, App, ParentElement, Styled, Window};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::{h_flex, v_flex, ActiveTheme, WindowExt};

use crate::platform::NotificationPermission;

#[cfg(target_os = "macos")]
const NOTIFICATION_SETTINGS_URL: &str =
    "x-apple.systempreferences:com.apple.Notifications-Settings.extension";

struct HelpCopy {
    status: &'static str,
    instructions: &'static str,
    recheck_label: &'static str,
}

fn help_copy(permission: NotificationPermission) -> HelpCopy {
    match permission {
        #[cfg(target_os = "macos")]
        NotificationPermission::Allowed => HelpCopy {
            status: "Notifications are allowed for PR Marmot.",
            instructions: "No system setting needs to be changed.",
            recheck_label: "Check again",
        },
        NotificationPermission::Denied => HelpCopy {
            status: "Notifications are turned off for PR Marmot.",
            instructions: if cfg!(target_os = "macos") {
                "In System Settings, open Notifications, choose PR Marmot, then turn on Allow Notifications."
            } else {
                "Open your desktop environment’s notification settings and allow notifications for PR Marmot. The exact location depends on your Linux desktop."
            },
            recheck_label: "Recheck permission",
        },
        #[cfg(target_os = "macos")]
        NotificationPermission::NotDetermined => HelpCopy {
            status: "PR Marmot has not asked for notification permission yet.",
            instructions: "Continue to let the operating system ask for permission. No notification will be sent by this check.",
            recheck_label: "Request permission",
        },
        #[cfg(target_os = "macos")]
        NotificationPermission::Unknown => HelpCopy {
            status: "PR Marmot could not determine the current notification permission.",
            instructions: if cfg!(target_os = "macos") {
                "Check System Settings > Notifications > PR Marmot, then try again."
            } else {
                "Check your desktop environment’s notification settings, then try again."
            },
            recheck_label: "Check again",
        },
        #[cfg(not(target_os = "macos"))]
        NotificationPermission::Unsupported => HelpCopy {
            status: "This desktop does not expose a notification permission check to PR Marmot.",
            instructions: "Notification controls vary by Linux desktop. Check your desktop environment’s notification settings and allow PR Marmot if it appears there.",
            recheck_label: "Try again",
        },
    }
}

/// Open notification-permission guidance in the existing GPUI dialog shell.
///
/// `on_recheck` runs only after the user clicks the final button. For
/// `NotDetermined`, the parent should call `Platform::request_notification_permission`;
/// for every other state it should call `Platform::check_notification_permission`.
/// The dialog closes before the callback runs so the parent can present the
/// result without stacking dialogs.
pub fn open_notification_help(
    window: &mut Window,
    cx: &mut App,
    permission: NotificationPermission,
    on_recheck: impl Fn(NotificationPermission, &mut Window, &mut App) + 'static,
) {
    let copy = help_copy(permission);
    let on_recheck = Rc::new(on_recheck);

    window.open_dialog(cx, move |dialog, _, cx| {
        let mut actions = h_flex().justify_end().gap_2().child(
            Button::new("notification-not-now")
                .label("Not now")
                .on_click(|_, window, cx| window.close_dialog(cx)),
        );

        #[cfg(target_os = "macos")]
        {
            actions = actions.child(
                Button::new("open-notification-settings")
                    .label("Open System Settings")
                    .on_click(|_, _, cx| cx.open_url(NOTIFICATION_SETTINGS_URL)),
            );
        }

        let on_recheck = on_recheck.clone();
        actions = actions.child(
            Button::new("recheck-notification-permission")
                .primary()
                .label(copy.recheck_label)
                .on_click(move |_, window, cx| {
                    window.close_dialog(cx);
                    on_recheck(permission, window, cx);
                }),
        );

        dialog
            .title("Allow desktop notifications")
            .w(px(500.))
            .close_button(true)
            .child(
                v_flex()
                    .gap_3()
                    .text_size(px(13.))
                    .child(div().text_color(cx.theme().foreground).child(copy.status))
                    .child(
                        div()
                            .text_color(cx.theme().muted_foreground)
                            .child(copy.instructions),
                    )
                    .child(
                        div()
                            .p_3()
                            .rounded(px(4.))
                            .bg(cx.theme().muted)
                            .text_color(cx.theme().muted_foreground)
                            .child(
                                "PR Marmot sends notifications only while the app is open. It does not install a background helper.",
                            ),
                    )
                    .child(actions),
            )
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn denied_copy_is_actionable() {
        let copy = help_copy(NotificationPermission::Denied);
        assert!(copy.status.contains("turned off"));
        assert!(
            copy.instructions.contains("notification settings")
                || copy.instructions.contains("Notifications")
        );
        assert_eq!(copy.recheck_label, "Recheck permission");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn undetermined_permission_asks_before_claiming_success() {
        let copy = help_copy(NotificationPermission::NotDetermined);
        assert_eq!(copy.recheck_label, "Request permission");
        assert!(!copy.status.contains("allowed"));
    }
}
