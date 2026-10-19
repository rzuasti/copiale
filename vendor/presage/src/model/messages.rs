use libsignal_service::{
    cipher::DecryptionFailure,
    prelude::Content,
    proto,
    protocol::{DeviceId, ServiceId},
};

#[derive(Debug)]
pub enum Received {
    /// when the receive loop is empty, happens when opening the websocket for the first time
    /// once you're done synchronizing all pending messages for this registered client.
    QueueEmpty,

    /// Got contacts (only applies if linked to a primary device
    /// Contacts can be later queried in the store.
    Contacts,

    /// A message from a known sender failed to decrypt. Send
    /// [`DecryptionFailure::retry_request`] with
    /// [`Manager::send_retry_request`](crate::Manager::send_retry_request)
    /// so the sender resends it.
    DecryptionError(Box<DecryptionFailure>),

    /// A device of `sender` couldn't decrypt our message sent at `timestamp` and asks for it
    /// again; resend it with [`Manager::resend_content`](crate::Manager::resend_content) if
    /// `sender_device` got it ([`SentContent::recipients`]).
    ///
    /// `session_archived` is set if our session with that device was the cause: it was archived,
    /// so the next message starts a new one. Without a message to resend, send a null message
    /// ([`Manager::send_null_message`](crate::Manager::send_null_message)) to start it.
    RetryRequest {
        sender: ServiceId,
        sender_device: DeviceId,
        timestamp: u64,
        session_archived: bool,
    },

    /// Incoming decrypted message with metadata and content
    Content(Box<Content>),
}

/// A message as it went out, to resend when a recipient asks
/// ([`Received::RetryRequest`]).
#[derive(Debug, Clone)]
pub struct SentContent {
    /// Who got it, apart from this account's other devices: the recipient,
    /// or the group members it reached, each with the devices it went to.
    /// Answer retry requests only from those devices.
    pub recipients: Vec<(ServiceId, Vec<DeviceId>)>,
    pub content: proto::Content,
}
