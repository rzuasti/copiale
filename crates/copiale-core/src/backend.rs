//! Boundary between the core and the Signal network.

use futures::stream::LocalBoxStream;

use crate::model::{IncomingMessage, MessageBody, ServiceId, Timestamp};

/// Content delivered by the backend.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Incoming {
    /// Queued messages have been delivered; later ones arrive live.
    QueueEmpty,
    Message(IncomingMessage),
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

    /// Sends a text message to one account.
    async fn send_text(
        &mut self,
        recipient: ServiceId,
        body: &MessageBody,
        timestamp: Timestamp,
    ) -> Result<(), BackendError>;
}
