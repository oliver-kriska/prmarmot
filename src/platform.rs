//! Thin platform adapters. Notifications are delivered only while this process
//! is running, and a click on one comes back to the UI with its canonical PR
//! id; there is no helper process or background daemon.

use std::cell::Cell;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

/// Notifications whose click is still awaited, and delivery or permission
/// workers running at once. On macOS a wait is a task the UI drops once this
/// many newer ones exist; on Linux it is a thread the notification server
/// releases when the notification expires.
pub const MAX_NOTIFICATION_WAITERS: usize = 32;
const DEMO_NOTIFICATION_DENIED: &str = "PRMARMOT_DEMO_NOTIFICATION_DENIED";
/// The notification's one button, and what a click on its body counts as.
const OPEN_ACTION: &str = "default";

struct NotificationPermit(Arc<AtomicUsize>);

impl NotificationPermit {
    fn acquire(count: &Arc<AtomicUsize>) -> Option<Self> {
        count
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                (value < MAX_NOTIFICATION_WAITERS).then_some(value + 1)
            })
            .ok()
            .map(|_| Self(count.clone()))
    }
}

impl Drop for NotificationPermit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotificationPermission {
    #[cfg(target_os = "macos")]
    Allowed,
    Denied,
    #[cfg(target_os = "macos")]
    NotDetermined,
    #[cfg(target_os = "macos")]
    Unknown,
    #[cfg(not(target_os = "macos"))]
    Unsupported,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotificationPermissionOperation {
    Check,
    Request,
}

impl NotificationPermissionOperation {
    /// What failed, as the footer says it: "Couldn't check notification
    /// permission: …".
    pub fn failure(self, message: &str) -> String {
        match self {
            Self::Check => format!("Couldn't check notification permission: {message}"),
            Self::Request => format!("Couldn't ask for notification permission: {message}"),
        }
    }
}

#[derive(Debug)]
pub enum PlatformEvent {
    /// A notification's "Open pull request": its PR id, and its URL for when
    /// the PR is no longer on the board.
    #[cfg(not(target_os = "macos"))]
    Clicked {
        pr_id: String,
        url: String,
    },
    /// A notification macOS accepted, whose click the UI now awaits.
    #[cfg(target_os = "macos")]
    Delivered(PendingClick),
    NotificationError(String),
    NotificationPermissionChanged(NotificationPermission),
    NotificationPermissionError {
        operation: NotificationPermissionOperation,
        message: String,
    },
}

/// A delivered notification still waiting for the person to act on it. The UI
/// awaits [`PendingClick::opened`] in a task it can drop: a dropped wait holds
/// no thread, and the notification stays in Notification Center (a click on it
/// then only brings the app forward). A wait that never ends (the person
/// clears the notification without a response) is therefore harmless.
#[cfg(target_os = "macos")]
#[derive(Debug)]
pub struct PendingClick {
    handle: mac_usernotifications::NotificationHandle,
    pr_id: String,
    url: String,
}

#[cfg(target_os = "macos")]
impl PendingClick {
    /// The PR id and URL when the person opened the pull request, from the
    /// notification itself or its button; `None` when they dismissed it.
    pub async fn opened(self) -> Option<(String, String)> {
        let response = self.handle.response().await.ok()?;
        (response.is_default_action() || response.action_identifier == OPEN_ACTION)
            .then_some((self.pr_id, self.url))
    }
}

/// Wakes the UI when a worker thread has queued a [`PlatformEvent`], so a
/// notification click or a permission answer is handled the moment it
/// arrives instead of on the window's next slow tick. A counter plus the
/// UI's waker: no runtime, no thread parked on the channel.
#[derive(Default)]
pub struct EventSignal {
    sent: AtomicU64,
    waker: Mutex<Option<Waker>>,
}

impl EventSignal {
    /// How many events have been queued so far; pass it to [`Self::changed`].
    pub fn seen(&self) -> u64 {
        self.sent.load(Ordering::Acquire)
    }

    /// Resolves once more events were queued than `seen` counts, with the new
    /// count. Resolves immediately when they already have.
    pub fn changed(self: &Arc<Self>, seen: u64) -> EventsQueued {
        EventsQueued {
            signal: self.clone(),
            seen,
        }
    }

    fn notify(&self) {
        self.sent.fetch_add(1, Ordering::AcqRel);
        if let Some(waker) = self.waker.lock().unwrap_or_else(|e| e.into_inner()).take() {
            waker.wake();
        }
    }
}

/// The future behind [`EventSignal::changed`].
pub struct EventsQueued {
    signal: Arc<EventSignal>,
    seen: u64,
}

impl Future for EventsQueued {
    type Output = u64;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<u64> {
        let now = self.signal.seen();
        if now != self.seen {
            return Poll::Ready(now);
        }
        *self.signal.waker.lock().unwrap_or_else(|e| e.into_inner()) = Some(cx.waker().clone());
        // A send between the first load and storing the waker found no waker
        // to wake; look again so it is never lost.
        let now = self.signal.seen();
        if now != self.seen {
            Poll::Ready(now)
        } else {
            Poll::Pending
        }
    }
}

/// The worker threads' end of the event queue: a bounded channel whose every
/// send also wakes the UI.
#[derive(Clone)]
struct EventSink {
    tx: SyncSender<PlatformEvent>,
    signal: Arc<EventSignal>,
}

impl EventSink {
    /// Queues the event and wakes the UI; `Err` when the queue is full (the
    /// event is dropped, as a full `sync_channel` dropped it before).
    fn try_send(&self, event: PlatformEvent) -> Result<(), ()> {
        self.tx.try_send(event).map_err(|_| ())?;
        self.signal.notify();
        Ok(())
    }
}

pub struct Platform {
    event_tx: EventSink,
    event_rx: Receiver<PlatformEvent>,
    workers: Arc<AtomicUsize>,
    /// The number on the Dock tile now, so a refresh that changes nothing
    /// doesn't ask AppKit to redraw it.
    shown_badge: Cell<Option<usize>>,
}

impl Platform {
    pub fn new() -> Self {
        let (tx, event_rx) = mpsc::sync_channel(MAX_NOTIFICATION_WAITERS);
        Self {
            event_tx: EventSink {
                tx,
                signal: Arc::default(),
            },
            event_rx,
            workers: Arc::new(AtomicUsize::new(0)),
            shown_badge: Cell::new(None),
        }
    }

    pub fn take_event(&self) -> Option<PlatformEvent> {
        self.event_rx.try_recv().ok()
    }

    /// What the UI awaits to learn that [`Self::take_event`] has something.
    pub fn event_signal(&self) -> Arc<EventSignal> {
        self.event_tx.signal.clone()
    }

    /// Query notification authorization on a bounded background worker.
    ///
    /// This never prompts and never sends a notification. The result arrives as
    /// a [`PlatformEvent::NotificationPermissionChanged`] or
    /// [`PlatformEvent::NotificationPermissionError`].
    pub fn check_notification_permission(&self) {
        if std::env::var_os(DEMO_NOTIFICATION_DENIED).is_some() {
            let _ = self
                .event_tx
                .try_send(PlatformEvent::NotificationPermissionChanged(
                    NotificationPermission::Denied,
                ));
            return;
        }
        self.spawn_permission_worker(
            NotificationPermissionOperation::Check,
            check_notification_permission_blocking,
        );
    }

    /// Ask the OS for notification authorization on a bounded background worker.
    ///
    /// This is the only permission API that may display the system prompt. It
    /// does not send a notification. Call it only in response to a user action.
    pub fn request_notification_permission(&self) {
        self.spawn_permission_worker(
            NotificationPermissionOperation::Request,
            request_notification_permission_blocking,
        );
    }

    fn spawn_permission_worker(
        &self,
        operation: NotificationPermissionOperation,
        work: fn() -> Result<NotificationPermission, String>,
    ) {
        let Some(permit) = NotificationPermit::acquire(&self.workers) else {
            let _ = self
                .event_tx
                .try_send(PlatformEvent::NotificationPermissionError {
                    operation,
                    message: "Notification permission check could not start because too many notification tasks are active".into(),
                });
            return;
        };
        let event_tx = self.event_tx.clone();
        std::thread::spawn(move || {
            let _permit = permit;
            let event = match work() {
                Ok(permission) => PlatformEvent::NotificationPermissionChanged(permission),
                Err(message) => PlatformEvent::NotificationPermissionError { operation, message },
            };
            let _ = event_tx.try_send(event);
        });
    }

    pub fn notify(&self, title: String, body: String, pr_id: String, url: String, sound: bool) {
        let Some(permit) = NotificationPermit::acquire(&self.workers) else {
            return;
        };
        let event_tx = self.event_tx.clone();
        std::thread::spawn(move || {
            let _permit = permit;
            #[cfg(target_os = "macos")]
            match check_notification_permission_blocking() {
                Ok(NotificationPermission::Allowed) => {}
                Ok(permission) => {
                    let _ =
                        event_tx.try_send(PlatformEvent::NotificationPermissionChanged(permission));
                    return;
                }
                Err(message) => {
                    let _ = event_tx.try_send(PlatformEvent::NotificationPermissionError {
                        operation: NotificationPermissionOperation::Check,
                        message,
                    });
                    return;
                }
            }
            #[cfg(target_os = "macos")]
            {
                let notification = mac_usernotifications::Notification::new()
                    .title(&title)
                    .message(&body)
                    .maybe_sound(sound.then_some("default"))
                    .action(mac_usernotifications::Action::button(
                        OPEN_ACTION,
                        "Open pull request",
                    ));
                // The worker ends once macOS has the notification; the click
                // is awaited by the UI, not by this thread.
                let event = match notification.send_blocking() {
                    Ok(handle) => PlatformEvent::Delivered(PendingClick { handle, pr_id, url }),
                    Err(error) => PlatformEvent::NotificationError(format!(
                        "Notification delivery failed: {error}"
                    )),
                };
                let _ = event_tx.try_send(event);
            }
            #[cfg(not(target_os = "macos"))]
            {
                let mut notification = notify_rust::Notification::new();
                notification
                    .appname("PR Marmot")
                    .summary(&title)
                    .body(&body)
                    .action(OPEN_ACTION, "Open pull request");
                if sound {
                    notification.sound_name("default");
                }
                match notification.show() {
                    Ok(handle) => {
                        handle.wait_for_action(move |action| {
                            if action == OPEN_ACTION {
                                let _ = event_tx.try_send(PlatformEvent::Clicked { pr_id, url });
                            }
                        });
                    }
                    Err(error) => {
                        let _ = event_tx.try_send(PlatformEvent::NotificationError(format!(
                            "Notification delivery failed: {error}"
                        )));
                    }
                }
            }
        });
    }

    pub fn set_badge(&self, count: usize, enabled: bool) {
        let count = if enabled { count } else { 0 };
        if self.shown_badge.replace(Some(count)) != Some(count) {
            set_badge(count);
        }
    }
}

#[cfg(target_os = "macos")]
fn check_notification_permission_blocking() -> Result<NotificationPermission, String> {
    mac_usernotifications::blocking::get_notification_settings()
        .map(|settings| permission_from_authorization_status(settings.authorization_status))
        .map_err(|error| format!("Could not check notification permission: {error}"))
}

#[cfg(target_os = "macos")]
fn permission_from_authorization_status(
    status: mac_usernotifications::AuthorizationStatus,
) -> NotificationPermission {
    use mac_usernotifications::AuthorizationStatus;
    match status {
        AuthorizationStatus::Authorized
        | AuthorizationStatus::Provisional
        | AuthorizationStatus::Ephemeral => NotificationPermission::Allowed,
        AuthorizationStatus::Denied => NotificationPermission::Denied,
        AuthorizationStatus::NotDetermined => NotificationPermission::NotDetermined,
        AuthorizationStatus::Unknown => NotificationPermission::Unknown,
    }
}

#[cfg(not(target_os = "macos"))]
fn check_notification_permission_blocking() -> Result<NotificationPermission, String> {
    Ok(NotificationPermission::Unsupported)
}

#[cfg(target_os = "macos")]
fn request_notification_permission_blocking() -> Result<NotificationPermission, String> {
    match mac_usernotifications::blocking::request_auth() {
        Ok(true) => Ok(NotificationPermission::Allowed),
        Ok(false) => Ok(NotificationPermission::Denied),
        Err(error) => Err(format!("Notification permission request failed: {error}")),
    }
}

#[cfg(not(target_os = "macos"))]
fn request_notification_permission_blocking() -> Result<NotificationPermission, String> {
    Ok(NotificationPermission::Unsupported)
}

#[cfg(target_os = "macos")]
fn set_badge(count: usize) {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSApplication;
    use objc2_foundation::NSString;

    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    let label = (count > 0).then(|| NSString::from_str(&count.to_string()));
    app.dockTile().setBadgeLabel(label.as_deref());
}

#[cfg(not(target_os = "macos"))]
fn set_badge(_count: usize) {}

/// Put a rich HTML flavour and its plain-text alternative on the clipboard in
/// one write, so each destination reads the one it renders best. GPUI's
/// clipboard is plain-text only, hence the direct pasteboard call. Returns
/// `false` where rich text is unsupported; the caller copies plain text.
#[cfg(target_os = "macos")]
pub fn write_rich_clipboard(html: &str, plain: &str) -> bool {
    use objc2_app_kit::{NSPasteboard, NSPasteboardTypeHTML, NSPasteboardTypeString};
    use objc2_foundation::NSString;

    let pasteboard = NSPasteboard::generalPasteboard();
    pasteboard.clearContents();
    // SAFETY: AppKit's pasteboard type constants are immutable static strings.
    let (html_type, string_type) = unsafe { (NSPasteboardTypeHTML, NSPasteboardTypeString) };
    // Richest flavour first: some readers take the first type they understand.
    pasteboard.setString_forType(&NSString::from_str(html), html_type)
        && pasteboard.setString_forType(&NSString::from_str(plain), string_type)
}

#[cfg(not(target_os = "macos"))]
pub fn write_rich_clipboard(_html: &str, _plain: &str) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_queued_event_wakes_the_ui_and_the_next_wait_stays_pending() {
        use std::task::Wake;

        struct Woken(AtomicUsize);
        impl Wake for Woken {
            fn wake(self: Arc<Self>) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }

        let platform = Platform::new();
        let signal = platform.event_signal();
        let woken = Arc::new(Woken(AtomicUsize::new(0)));
        let waker = Waker::from(woken.clone());
        let mut cx = Context::from_waker(&waker);

        let seen = signal.seen();
        let mut wait = signal.changed(seen);
        assert_eq!(Pin::new(&mut wait).poll(&mut cx), Poll::Pending);

        platform
            .event_tx
            .try_send(PlatformEvent::NotificationError("x".into()))
            .unwrap();
        assert_eq!(woken.0.load(Ordering::SeqCst), 1, "the send wakes the UI");
        let Poll::Ready(now) = Pin::new(&mut wait).poll(&mut cx) else {
            panic!("an event was queued");
        };
        assert!(platform.take_event().is_some());
        assert!(platform.take_event().is_none());

        // Nothing new: the next wait parks until another send.
        let mut wait = signal.changed(now);
        assert_eq!(Pin::new(&mut wait).poll(&mut cx), Poll::Pending);
        drop(wait);
        // A send with the waker already taken (or never stored) is still counted.
        platform
            .event_tx
            .try_send(PlatformEvent::NotificationError("y".into()))
            .unwrap();
        assert_eq!(
            Pin::new(&mut signal.changed(now)).poll(&mut cx),
            Poll::Ready(now + 1)
        );
    }

    #[test]
    fn workers_are_bounded_and_released_on_early_return() {
        let count = Arc::new(AtomicUsize::new(0));
        let mut permits: Vec<_> = (0..MAX_NOTIFICATION_WAITERS)
            .map(|_| NotificationPermit::acquire(&count).unwrap())
            .collect();
        assert!(NotificationPermit::acquire(&count).is_none());
        permits.pop();
        let replacement = NotificationPermit::acquire(&count).unwrap();
        assert!(NotificationPermit::acquire(&count).is_none());
        drop(replacement);
        drop(permits);
        assert_eq!(count.load(Ordering::Relaxed), 0);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_authorization_statuses_are_typed() {
        use mac_usernotifications::AuthorizationStatus;
        for (status, permission) in [
            (
                AuthorizationStatus::Authorized,
                NotificationPermission::Allowed,
            ),
            (
                AuthorizationStatus::Provisional,
                NotificationPermission::Allowed,
            ),
            (
                AuthorizationStatus::Ephemeral,
                NotificationPermission::Allowed,
            ),
            (AuthorizationStatus::Denied, NotificationPermission::Denied),
            (
                AuthorizationStatus::NotDetermined,
                NotificationPermission::NotDetermined,
            ),
            (
                AuthorizationStatus::Unknown,
                NotificationPermission::Unknown,
            ),
        ] {
            assert_eq!(permission_from_authorization_status(status), permission);
        }
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn linux_permission_query_has_an_explicit_fallback() {
        assert_eq!(
            check_notification_permission_blocking().unwrap(),
            NotificationPermission::Unsupported
        );
        assert_eq!(
            request_notification_permission_blocking().unwrap(),
            NotificationPermission::Unsupported
        );
    }
}
