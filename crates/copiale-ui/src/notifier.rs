//! Desktop notifications: the freedesktop D-Bus service on Linux and the
//! BSDs, through notify-rust; UNUserNotificationCenter on macOS, which
//! needs the app bundle; WinRT toasts on Windows.

#[cfg(windows)]
mod toast;

use futures::StreamExt as _;
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};

use crate::chat::{NoticeAction, NoticeKey};

/// Where notification actions go: the desktop, or a fake in tests.
pub trait Notifier {
    fn apply(&self, action: NoticeAction);
}

/// Drops every notice.
pub struct NoNotifier;

impl Notifier for NoNotifier {
    fn apply(&self, _: NoticeAction) {}
}

/// Shows notices on the desktop from a thread of its own; clicked ones
/// arrive on the channel from [`DesktopNotifier::new`].
pub struct DesktopNotifier {
    requests: UnboundedSender<NoticeAction>,
}

impl DesktopNotifier {
    pub fn new() -> std::io::Result<(Self, UnboundedReceiver<NoticeKey>)> {
        let (requests, received) = unbounded();
        let (clicks, clicked) = unbounded();
        std::thread::Builder::new()
            .name("copiale-notify".into())
            .spawn(move || serve(received, clicks))?;
        Ok((Self { requests }, clicked))
    }
}

impl Notifier for DesktopNotifier {
    fn apply(&self, action: NoticeAction) {
        self.requests.unbounded_send(action).ok();
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn notification(notice: &crate::chat::Notice) -> notify_rust::Notification {
    let mut notification = notify_rust::Notification::new();
    notification
        .appname("Copiale")
        .summary(&notice.title)
        .body(&escape(&notice.body))
        .action("default", "Open")
        .hint(notify_rust::Hint::DesktopEntry(crate::APP_ID.into()))
        .hint(notify_rust::Hint::Category("im.received".into()));
    if notice.quiet {
        notification
            .urgency(notify_rust::Urgency::Low)
            .hint(notify_rust::Hint::SuppressSound(true));
    }
    notification
}

/// Message text as plain text: freedesktop servers may read the body as
/// markup.
#[cfg(all(unix, not(target_os = "macos")))]
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Shows, replaces and closes notifications by key, and reports clicks.
#[cfg(all(unix, not(target_os = "macos")))]
fn serve(mut requests: UnboundedReceiver<NoticeAction>, clicks: UnboundedSender<NoticeKey>) {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;

    use futures::FutureExt as _;
    use futures::executor::LocalPool;
    use futures::future::{AbortHandle, abortable};
    use futures::task::LocalSpawnExt as _;
    use notify_rust::{NotificationHandle, NotificationResponse};

    struct Shown {
        handle: Rc<NotificationHandle>,
        waiter: AbortHandle,
    }
    type ShownMap = Rc<RefCell<HashMap<NoticeKey, Shown>>>;

    /// Reports a click, then forgets the notification unless replaced.
    async fn wait(
        handle: Rc<NotificationHandle>,
        key: NoticeKey,
        clicks: UnboundedSender<NoticeKey>,
        shown: ShownMap,
    ) {
        handle
            .wait_for_action_async(|response| {
                if let NotificationResponse::Default | NotificationResponse::Action(_) = response {
                    clicks.unbounded_send(key).ok();
                }
            })
            .await;
        let mut shown = shown.borrow_mut();
        if shown
            .get(&key)
            .is_some_and(|current| Rc::ptr_eq(&current.handle, &handle))
        {
            shown.remove(&key);
        }
    }

    let mut pool = LocalPool::new();
    let spawner = pool.spawner();
    pool.run_until(async move {
        let shown = ShownMap::default();
        while let Some(action) = requests.next().await {
            match action {
                NoticeAction::Show(notice) => {
                    let mut notification = notification(&notice);
                    let previous = shown.borrow_mut().remove(&notice.key);
                    if let Some(previous) = previous {
                        // Same ID: the server replaces it in place.
                        notification.id(previous.handle.id());
                        previous.waiter.abort();
                    }
                    match notification.show_async().await {
                        Ok(handle) => {
                            let handle = Rc::new(handle);
                            let waiter =
                                wait(handle.clone(), notice.key, clicks.clone(), shown.clone());
                            let (waiter, abort) = abortable(waiter);
                            if spawner.spawn_local(waiter.map(drop)).is_ok() {
                                shown.borrow_mut().insert(
                                    notice.key,
                                    Shown {
                                        handle,
                                        waiter: abort,
                                    },
                                );
                            }
                        }
                        Err(error) => tracing::warn!(%error, "showing a notification failed"),
                    }
                }
                NoticeAction::Close(key) => {
                    let previous = shown.borrow_mut().remove(&key);
                    if let Some(previous) = previous {
                        previous.waiter.abort();
                        previous.handle.close_async().await;
                    }
                }
            }
        }
    });
}

/// Shows, replaces and closes notifications by key, and reports clicks.
#[cfg(target_os = "macos")]
fn serve(mut requests: UnboundedReceiver<NoticeAction>, clicks: UnboundedSender<NoticeKey>) {
    use std::collections::HashMap;

    use futures::FutureExt as _;
    use futures::executor::LocalPool;
    use futures::future::{AbortHandle, abortable};
    use futures::task::LocalSpawnExt as _;
    use mac_usernotifications::{
        InterruptionLevel, Notification, check_bundle, close_delivered, request_auth,
    };

    // UNUserNotificationCenter crashes without a bundle identifier.
    if let Err(error) = check_bundle() {
        tracing::warn!(%error, "notifications need the app bundle");
        return;
    }
    let mut pool = LocalPool::new();
    let spawner = pool.spawner();
    pool.run_until(async move {
        // Asked once, at the first notice.
        let mut allowed = None;
        let mut waiters = HashMap::<NoticeKey, AbortHandle>::new();
        while let Some(action) = requests.next().await {
            match action {
                NoticeAction::Show(notice) => {
                    if allowed.is_none() {
                        allowed = Some(request_auth().await.unwrap_or_else(|error| {
                            tracing::warn!(%error, "notification permission failed");
                            false
                        }));
                    }
                    if allowed != Some(true) {
                        continue;
                    }
                    if let Some(waiter) = waiters.remove(&notice.key) {
                        waiter.abort();
                    }
                    // Same identifier: replaced in place.
                    let mut notification = Notification::new()
                        .id(&identifier(notice.key))
                        .title(&notice.title)
                        .message(&notice.body);
                    notification = if notice.quiet {
                        notification.interruption_level(InterruptionLevel::Passive)
                    } else {
                        notification.default_sound()
                    };
                    match notification.send().await {
                        Ok(handle) => {
                            let clicks = clicks.clone();
                            let key = notice.key;
                            let (waiter, abort) = abortable(async move {
                                if let Ok(response) = handle.response().await
                                    && response.is_default_action()
                                {
                                    clicks.unbounded_send(key).ok();
                                }
                            });
                            if spawner.spawn_local(waiter.map(drop)).is_ok() {
                                waiters.insert(key, abort);
                            }
                        }
                        Err(error) => tracing::warn!(%error, "showing a notification failed"),
                    }
                }
                NoticeAction::Close(key) => {
                    if let Some(waiter) = waiters.remove(&key) {
                        waiter.abort();
                    }
                    close_delivered(&identifier(key)).await;
                }
            }
        }
    });
}

/// The notification's identifier for `key` (a toast's tag on Windows).
#[cfg(any(target_os = "macos", windows))]
fn identifier(key: NoticeKey) -> String {
    match key {
        NoticeKey::Chat(conversation) => format!("chat-{}", conversation.get()),
        NoticeKey::Backlog => "backlog".into(),
    }
}

#[cfg(windows)]
use toast::serve;

/// A toast's XML: the notice's title and body, silent if quiet.
#[cfg(any(windows, test))]
fn toast_xml(notice: &crate::chat::Notice) -> String {
    let audio = if notice.quiet {
        r#"<audio silent="true"/>"#
    } else {
        ""
    };
    format!(
        r#"<toast><visual><binding template="ToastGeneric"><text>{}</text><text>{}</text></binding></visual>{audio}</toast>"#,
        xml_text(&notice.title),
        xml_text(&notice.body),
    )
}

/// `text` as XML character data, without the characters XML can't hold.
#[cfg(any(windows, test))]
fn xml_text(text: &str) -> String {
    text.chars()
        .filter(|&c| {
            matches!(c, '\t' | '\n' | '\r' | ' '..='\u{d7ff}' | '\u{e000}'..='\u{fffd}')
                || c >= '\u{10000}'
        })
        .collect::<String>()
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::Notice;

    fn notice(title: &str, body: &str, quiet: bool) -> Notice {
        Notice {
            key: NoticeKey::Backlog,
            title: title.into(),
            body: body.into(),
            quiet,
        }
    }

    #[test]
    fn toasts_hold_plain_text() {
        assert_eq!(
            toast_xml(&notice("A&B", "<b>hi</b>\u{1}\u{fffe} 👋\n", false)),
            "<toast><visual><binding template=\"ToastGeneric\"><text>A&amp;B</text>\
             <text>&lt;b&gt;hi&lt;/b&gt; 👋\n</text></binding></visual></toast>"
        );
    }

    #[test]
    fn quiet_toasts_are_silent() {
        assert!(toast_xml(&notice("a", "b", true)).ends_with(r#"<audio silent="true"/></toast>"#));
        assert!(!toast_xml(&notice("a", "b", false)).contains("<audio"));
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn bodies_are_plain_text() {
        assert_eq!(
            escape("<a href='x'>R&D</a>"),
            "&lt;a href='x'&gt;R&amp;D&lt;/a&gt;"
        );
    }
}
