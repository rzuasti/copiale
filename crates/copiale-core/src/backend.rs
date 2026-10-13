//! Boundary between the core and the Signal network.

use futures::future::LocalBoxFuture;
use futures::stream::LocalBoxStream;
use tokio::sync::oneshot;

use crate::model::{IncomingMessage, MessageBody, SentMessage, ServiceId, Timestamp};
use crate::storage::Database;

/// Content delivered by the backend.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Incoming {
    /// Queued messages have been delivered; later ones arrive live.
    QueueEmpty,
    Message(IncomingMessage),
    /// Sent by this account from another device.
    Sent(SentMessage),
    /// Contacts synced from the phone are now in the store.
    ContactsSynced,
}

/// Backend failure, as text for display. Must not contain message content.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct BackendError(String);

impl BackendError {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

/// Signal network access: presage in the app, a fake in tests.
///
/// Futures need not be `Send`: presage's are not, so the core drives the
/// backend from a single thread.
#[allow(async_fn_in_trait)]
pub trait SignalBackend {
    /// Opens the connection and streams incoming content until it closes.
    async fn receive(&mut self) -> Result<LocalBoxStream<'static, Incoming>, BackendError>;

    /// Sends a text message to one account, with a copy to this account's
    /// other devices.
    ///
    /// The future doesn't borrow the backend, so the core can keep serving
    /// while it runs.
    fn send_text(
        &mut self,
        recipient: ServiceId,
        body: MessageBody,
        timestamp: Timestamp,
    ) -> LocalBoxFuture<'static, Result<(), BackendError>>;

    /// Asks the phone to send its contacts; they arrive as
    /// [`Incoming::ContactsSynced`].
    async fn request_contact_sync(&mut self) -> Result<(), BackendError>;
}

/// Account setup: presage in the app, a fake in tests.
#[allow(async_fn_in_trait)]
pub trait Linker {
    type Backend: SignalBackend;

    /// The account linked in `db`, or `None` if there is none yet.
    async fn load(&mut self, db: &Database) -> Result<Option<Self::Backend>, BackendError>;

    /// Links as a secondary device. Sends the provisioning URL, to show as
    /// a QR code, on `url`; completes once the phone confirms.
    async fn link(
        &mut self,
        db: &Database,
        device_name: &str,
        url: oneshot::Sender<String>,
    ) -> Result<Self::Backend, BackendError>;
}
