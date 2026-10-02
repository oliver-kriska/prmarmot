//! The window's slow tick, and the task that handles the platform's events:
//! notification clicks, delivery failures and permission changes.

use super::*;
use crate::platform::PlatformEvent;

impl RootView {
    /// The "synced Xm ago" label and the wait ages render only on notify —
    /// without a slow tick they can claim "just now" for a whole refresh
    /// interval. The tick runs every 5 s so timed snoozes return promptly,
    /// but it repaints only when the synced label changes (once a minute) or
    /// while a rate-limit countdown is showing. An idle window draws one
    /// frame a minute; nothing animates. Platform events have their own task,
    /// [`Self::start_event_pump`], woken by the event itself.
    pub(super) fn start_ticker(window: &mut Window, cx: &mut Context<Self>) {
        let ticker = cx.entity().downgrade();
        let mut shown_sync_label = None;
        cx.spawn_in(window, async move |_this, cx| loop {
            cx.background_executor().timer(Duration::from_secs(5)).await;
            let Some(view) = ticker.upgrade() else { break };
            let _ = view.update_in(cx, |this, window, cx| {
                let selected = this.selected_row_id(cx);
                this.state.update(cx, |state, cx| {
                    state.set_focused_selection(selected, window.is_window_active());
                    state.wake_timed_snoozes(cx);
                });
                let state = this.state.read(cx);
                let sync_label = state
                    .last_synced
                    .map(|t| relative((Local::now() - t).num_seconds()));
                let counting_down = state.backoff_remaining().is_some();
                let label_changed = sync_label != shown_sync_label;
                shown_sync_label = sync_label;
                if !(label_changed || counting_down) {
                    return;
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Handles platform events as they arrive: a worker thread that queues
    /// one wakes this task, which drains the queue on the UI thread. Before,
    /// a click on a notification or the answer to a permission prompt waited
    /// for the next 5 s tick.
    pub(super) fn start_event_pump(
        signal: std::sync::Arc<crate::platform::EventSignal>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let view = cx.entity().downgrade();
        cx.spawn_in(window, async move |_this, cx| {
            let mut seen = signal.seen();
            loop {
                seen = signal.changed(seen).await;
                let Some(view) = view.upgrade() else { break };
                let handled = view.update_in(cx, |this, window, cx| {
                    let events: Vec<_> =
                        std::iter::from_fn(|| this.state.read(cx).take_platform_event()).collect();
                    for event in events {
                        this.handle_platform_event(event, window, cx);
                    }
                    cx.notify();
                });
                if handled.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    fn handle_platform_event(
        &mut self,
        event: PlatformEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            #[cfg(not(target_os = "macos"))]
            PlatformEvent::Clicked { pr_id, url } => {
                window.activate_window();
                self.select_notification_pr(pr_id, url, cx);
            }
            #[cfg(target_os = "macos")]
            PlatformEvent::Delivered(pending) => {
                self.await_notification_click(pending, window, cx);
            }
            PlatformEvent::NotificationError(error) => {
                self.state
                    .update(cx, |state, _| state.notification_error = Some(error));
            }
            PlatformEvent::NotificationPermissionChanged(permission) => {
                #[cfg(target_os = "macos")]
                if permission == crate::platform::NotificationPermission::Allowed {
                    self.state
                        .update(cx, |state, _| state.notification_error = None);
                    self.show_feedback("Notifications are allowed", cx);
                    return;
                }
                self.state.update(cx, |state, _| {
                    state.notification_error = Some("Notifications need attention".into());
                });
                if !self.notification_help_shown {
                    self.notification_help_shown = true;
                    self.show_notification_help(permission, window, cx);
                }
            }
            PlatformEvent::NotificationPermissionError { operation, message } => {
                self.state.update(cx, |state, _| {
                    state.notification_error = Some(operation.failure(&message));
                });
            }
        }
    }
}
