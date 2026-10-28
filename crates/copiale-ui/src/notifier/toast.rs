//! WinRT toasts, tagged by notice key so they can be replaced and closed.

use std::collections::HashMap;

use futures::StreamExt as _;
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender};
use windows::ApplicationModel::Package;
use windows::Data::Xml::Dom::XmlDocument;
use windows::Foundation::TypedEventHandler;
use windows::UI::Notifications::{ToastNotification, ToastNotificationManager, ToastNotifier};
use windows::core::{HSTRING, Result};

use super::{identifier, toast_xml};
use crate::chat::{Notice, NoticeAction, NoticeKey};

/// Borrowed while unpackaged, without an app ID of our own; the MSIX
/// package gives one (#100).
const POWERSHELL_APP_ID: &str =
    "{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\\WindowsPowerShell\\v1.0\\powershell.exe";
const GROUP: &str = "copiale";

struct Toasts {
    notifier: ToastNotifier,
    /// `None` when packaged: the package's is used.
    app_id: Option<HSTRING>,
    /// Kept so their click handlers stay registered.
    shown: HashMap<NoticeKey, ToastNotification>,
}

impl Toasts {
    fn new() -> Result<Self> {
        let (notifier, app_id) = if Package::Current().is_ok() {
            (ToastNotificationManager::CreateToastNotifier()?, None)
        } else {
            let app_id = HSTRING::from(POWERSHELL_APP_ID);
            let notifier = ToastNotificationManager::CreateToastNotifierWithId(&app_id)?;
            (notifier, Some(app_id))
        };
        Ok(Self {
            notifier,
            app_id,
            shown: HashMap::new(),
        })
    }

    /// The same tag and group replace a toast in place.
    fn show(&mut self, notice: &Notice, clicks: UnboundedSender<NoticeKey>) -> Result<()> {
        let xml = XmlDocument::new()?;
        xml.LoadXml(&HSTRING::from(toast_xml(notice)))?;
        let toast = ToastNotification::CreateToastNotification(&xml)?;
        toast.SetTag(&HSTRING::from(identifier(notice.key)))?;
        toast.SetGroup(&HSTRING::from(GROUP))?;
        let key = notice.key;
        toast.Activated(&TypedEventHandler::new(move |_, _| {
            clicks.unbounded_send(key).ok();
            Ok(())
        }))?;
        self.notifier.Show(&toast)?;
        self.shown.insert(key, toast);
        Ok(())
    }

    /// From the screen and the notification center.
    fn close(&mut self, key: NoticeKey) -> Result<()> {
        if let Some(toast) = self.shown.remove(&key) {
            // Fails once the toast has left the screen.
            self.notifier.Hide(&toast).ok();
        }
        let history = ToastNotificationManager::History()?;
        let (tag, group) = (HSTRING::from(identifier(key)), HSTRING::from(GROUP));
        match &self.app_id {
            Some(app_id) => history.RemoveGroupedTagWithId(&tag, &group, app_id),
            None => history.RemoveGroupedTag(&tag, &group),
        }
    }
}

/// Shows, replaces and closes notifications by key, and reports clicks.
pub(super) fn serve(
    mut requests: UnboundedReceiver<NoticeAction>,
    clicks: UnboundedSender<NoticeKey>,
) {
    let mut toasts = match Toasts::new() {
        Ok(toasts) => toasts,
        Err(error) => {
            tracing::warn!(%error, "notifications unavailable");
            return;
        }
    };
    futures::executor::block_on(async move {
        while let Some(action) = requests.next().await {
            let result = match action {
                NoticeAction::Show(notice) => toasts.show(&notice, clicks.clone()),
                NoticeAction::Close(key) => toasts.close(key),
            };
            if let Err(error) = result {
                tracing::warn!(%error, "a notification failed");
            }
        }
    });
}
