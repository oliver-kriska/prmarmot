//! Thin platform adapters. Notifications are delivered only while this process
//! is running. Their response waiter sends canonical PR ids back to the UI;
//! there is no helper process or background daemon.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::Arc;

const MAX_NOTIFICATION_WAITERS: usize = 32;
const DEMO_NOTIFICATION_DENIED: &str = "PRMARMOT_DEMO_NOTIFICATION_DENIED";

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

#[derive(Debug)]
pub enum PlatformEvent {
    Clicked(String),
    NotificationError(String),
    NotificationPermissionChanged(NotificationPermission),
    NotificationPermissionError {
        operation: NotificationPermissionOperation,
        message: String,
    },
}

pub struct Platform {
    event_tx: SyncSender<PlatformEvent>,
    event_rx: Receiver<PlatformEvent>,
    workers: Arc<AtomicUsize>,
}

impl Platform {
    pub fn new() -> Self {
        let (event_tx, event_rx) = mpsc::sync_channel(MAX_NOTIFICATION_WAITERS);
        Self {
            event_tx,
            event_rx,
            workers: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub fn take_event(&self) -> Option<PlatformEvent> {
        self.event_rx.try_recv().ok()
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

    pub fn notify(&self, title: String, body: String, pr_id: String, sound: bool) {
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
            let mut notification = notify_rust::Notification::new();
            notification
                .appname("PR Marmot")
                .summary(&title)
                .body(&body)
                .action("default", "Open pull request");
            if sound {
                notification.sound_name("default");
            }
            match notification.show() {
                Ok(handle) => {
                    handle.wait_for_action(move |action| {
                        if action == "default" {
                            let _ = event_tx.try_send(PlatformEvent::Clicked(pr_id));
                        }
                    });
                }
                Err(error) => {
                    let _ = event_tx.try_send(PlatformEvent::NotificationError(format!(
                        "Notification delivery failed: {error}"
                    )));
                }
            }
        });
    }

    pub fn set_badge(&self, count: usize, enabled: bool) {
        set_badge(if enabled { count } else { 0 });
    }
}

#[cfg(target_os = "macos")]
fn check_notification_permission_blocking() -> Result<NotificationPermission, String> {
    notify_rust::get_notification_settings_blocking()
        // notify-rust exposes the settings value but not its dependency's
        // AuthorizationStatus type. The crate is exactly pinned, so normalize
        // its documented Debug names here rather than adding a duplicate dep.
        .map(|settings| {
            permission_from_authorization_status(&format!("{:?}", settings.authorization_status))
        })
        .map_err(|error| format!("Could not check notification permission: {error}"))
}

#[cfg(target_os = "macos")]
fn permission_from_authorization_status(status: &str) -> NotificationPermission {
    match status {
        "Authorized" | "Provisional" | "Ephemeral" => NotificationPermission::Allowed,
        "Denied" => NotificationPermission::Denied,
        "NotDetermined" => NotificationPermission::NotDetermined,
        _ => NotificationPermission::Unknown,
    }
}

#[cfg(not(target_os = "macos"))]
fn check_notification_permission_blocking() -> Result<NotificationPermission, String> {
    Ok(NotificationPermission::Unsupported)
}

#[cfg(target_os = "macos")]
fn request_notification_permission_blocking() -> Result<NotificationPermission, String> {
    match notify_rust::request_auth_blocking() {
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
        assert_eq!(
            permission_from_authorization_status("Authorized"),
            NotificationPermission::Allowed
        );
        assert_eq!(
            permission_from_authorization_status("Provisional"),
            NotificationPermission::Allowed
        );
        assert_eq!(
            permission_from_authorization_status("Denied"),
            NotificationPermission::Denied
        );
        assert_eq!(
            permission_from_authorization_status("NotDetermined"),
            NotificationPermission::NotDetermined
        );
        assert_eq!(
            permission_from_authorization_status("future-status"),
            NotificationPermission::Unknown
        );
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
