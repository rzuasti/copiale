//! Desktop notifications through notify-rust: the freedesktop D-Bus service
//! on Linux and the BSDs, the notification center on macOS, WinRT toasts on
//! Windows.

use futures::StreamExt as _;
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use notify_rust::Notification;

use crate::chat::{Notice, NoticeAction, NoticeKey};

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

fn notification(notice: &Notice) -> Notification {
    let mut notification = Notification::new();
    notification
        .appname("Copiale")
        .summary(&notice.title)
        .body(&escape(&notice.body))
        .action("default", "Open");
    #[cfg(all(unix, not(target_os = "macos")))]
    notification
        .hint(notify_rust::Hint::DesktopEntry(crate::APP_ID.into()))
        .hint(notify_rust::Hint::Category("im.received".into()));
    notification
}

/// Message text as plain text: freedesktop servers may read the body as
/// markup.
fn escape(text: &str) -> String {
    if cfg!(all(unix, not(target_os = "macos"))) {
        text.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    } else {
        text.into()
    }
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

/// Shows notifications and reports clicks; replacing and closing them
/// await the macOS and Windows checks (#28).
#[cfg(not(all(unix, not(target_os = "macos"))))]
fn serve(mut requests: UnboundedReceiver<NoticeAction>, clicks: UnboundedSender<NoticeKey>) {
    futures::executor::block_on(async move {
        while let Some(action) = requests.next().await {
            let NoticeAction::Show(notice) = action else {
                continue;
            };
            let clicks = clicks.clone();
            std::thread::spawn(move || match notification(&notice).show() {
                Ok(handle) => handle.wait_for_action(|action| {
                    if action != "__closed" {
                        clicks.unbounded_send(notice.key).ok();
                    }
                }),
                Err(error) => tracing::warn!(%error, "showing a notification failed"),
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(all(unix, not(target_os = "macos")))]
    fn bodies_are_plain_text() {
        assert_eq!(
            escape("<a href='x'>R&D</a>"),
            "&lt;a href='x'&gt;R&amp;D&lt;/a&gt;"
        );
    }
}
