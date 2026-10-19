//! Boundary between the core and the Signal network.

use std::fmt;

use futures::future::LocalBoxFuture;
use futures::stream::LocalBoxStream;
use tokio::sync::oneshot;

use crate::model::{
    ChatRef, Destination, GroupId, GroupInfo, GroupMasterKey, IdentityKey, IncomingMessage,
    MessageChange, MessageRef, Outgoing, ReceiptKind, SafetyNumber, SentMessage, ServiceId,
    Timestamp,
};
use crate::storage::Database;

/// Content delivered by the backend.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Incoming {
    /// Queued messages have been delivered; later ones arrive live.
    QueueEmpty,
    Message(IncomingMessage),
    /// Sent by this account from another device.
    Sent(SentMessage),
    /// An edit or delete of an earlier message.
    Change(MessageChange),
    /// Messages and whole conversations deleted on another of this
    /// account's devices, for this account only.
    DeletedForMe {
        messages: Vec<MessageRef>,
        conversations: Vec<ChatRef>,
    },
    /// Contacts synced from the phone are now in the store.
    ContactsSynced,
    /// `sender` got or read our messages sent at `timestamps`.
    Receipt {
        sender: ServiceId,
        kind: ReceiptKind,
        timestamps: Vec<Timestamp>,
    },
    /// Messages, by author and sent time, read on another of this
    /// account's devices at `at`.
    ReadElsewhere {
        read: Vec<(ServiceId, Timestamp)>,
        at: Timestamp,
    },
    /// `sender` started or stopped typing, in a group if `group` is set.
    Typing {
        sender: ServiceId,
        group: Option<GroupId>,
        typing: bool,
    },
    /// The phone marked `peer`'s identity `key` verified, or `peer`
    /// unverified.
    Verified {
        peer: ServiceId,
        key: IdentityKey,
        verified: bool,
    },
    /// Settings synced from the phone; `None` fields are unchanged.
    Settings {
        read_receipts: Option<bool>,
        typing_indicators: Option<bool>,
    },
    /// A message couldn't be decrypted; asking for it again may get it
    /// resent.
    Undecryptable(Undecryptable),
    /// `sender`'s device `device` couldn't decrypt our message sent at
    /// `sent_at` and asks for it again. With `session_reset`, our session
    /// with that device was broken and is gone: the next message to them
    /// starts a new one.
    RetryRequested {
        sender: ServiceId,
        device: u32,
        sent_at: Timestamp,
        session_reset: bool,
    },
}

/// An account's identity key as sessions with it use it, and its safety
/// number with this account.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerIdentity {
    pub key: IdentityKey,
    pub safety_number: SafetyNumber,
}

/// A message that couldn't be decrypted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Undecryptable {
    pub sender: ServiceId,
    /// The sender's device it came from.
    pub device: u32,
    pub sent_at: Timestamp,
    /// The group it was sent in, if the sender said.
    pub group: Option<GroupId>,
    /// Whether it may have been a message to show, rather than content
    /// that never is, like receipts and typing.
    pub shown: bool,
    /// Asks the sender to resend it: [`SignalBackend::send_retry_request`].
    pub retry: RetryRequest,
}

/// A request to resend a message, as the backend encodes it, with the
/// group it was sent in. `Debug` hides it.
#[derive(Clone, PartialEq, Eq)]
pub struct RetryRequest {
    message: Vec<u8>,
    group_id: Option<Vec<u8>>,
}

impl RetryRequest {
    pub fn new(message: Vec<u8>, group_id: Option<Vec<u8>>) -> Self {
        Self { message, group_id }
    }

    pub fn message(&self) -> &[u8] {
        &self.message
    }

    pub fn group_id(&self) -> Option<&[u8]> {
        self.group_id.as_deref()
    }
}

impl fmt::Debug for RetryRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RetryRequest(…)")
    }
}

/// A message as it went out, to send again when a recipient can't decrypt
/// it. `Debug` hides the content.
#[derive(Clone, PartialEq, Eq)]
pub struct Resendable {
    /// Who got it, this account's other devices aside: empty for Note to
    /// Self.
    pub recipients: Vec<ServiceId>,
    /// As the backend encodes it.
    pub content: Vec<u8>,
}

impl fmt::Debug for Resendable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Resendable")
            .field("recipients", &self.recipients)
            .field("content_bytes", &self.content.len())
            .finish()
    }
}

/// Backend failure, as text for display. Must not contain message content.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct BackendError {
    message: String,
    kind: ErrorKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ErrorKind {
    Other,
    Network,
    Unauthorized,
    UntrustedIdentity,
}

impl BackendError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            kind: ErrorKind::Other,
        }
    }

    /// The connection failed; trying again later may work.
    pub fn network(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            kind: ErrorKind::Network,
        }
    }

    /// The server rejected this device's credentials, as once the phone
    /// unlinks it.
    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            kind: ErrorKind::Unauthorized,
        }
    }

    /// A recipient's verified identity key changed: sending to them waits
    /// until the user confirms the new one.
    pub fn untrusted_identity() -> Self {
        Self {
            message: "safety number changed; confirm it to send".into(),
            kind: ErrorKind::UntrustedIdentity,
        }
    }

    pub fn is_network(&self) -> bool {
        self.kind == ErrorKind::Network
    }

    pub fn is_unauthorized(&self) -> bool {
        self.kind == ErrorKind::Unauthorized
    }

    pub fn is_untrusted_identity(&self) -> bool {
        self.kind == ErrorKind::UntrustedIdentity
    }
}

/// Signal network access: presage in the app, a fake in tests.
///
/// Futures need not be `Send`: presage's are not, so the core drives the
/// backend from a single thread.
pub trait SignalBackend {
    /// Opens the connection and streams incoming content until it closes.
    ///
    /// The future doesn't borrow the backend, so the core can keep serving
    /// while it connects.
    fn receive(
        &mut self,
    ) -> LocalBoxFuture<'static, Result<LocalBoxStream<'static, Incoming>, BackendError>>;

    /// Ends the stream from [`Self::receive`] once what was already received
    /// has come through: the server may count it as delivered, so dropping
    /// the stream instead would lose it.
    fn stop_receiving(&mut self);

    /// Sends a message, or a change to one of ours, to one account or a
    /// group, with a copy to this account's other devices; returns it as it
    /// went out. Quotes of our messages have no author.
    ///
    /// The future doesn't borrow the backend, so the core can keep serving
    /// while it runs.
    fn send(
        &mut self,
        destination: Destination,
        outgoing: Outgoing,
        timestamp: Timestamp,
    ) -> LocalBoxFuture<'static, Result<Resendable, BackendError>>;

    /// Asks `sender` to resend a message that couldn't be decrypted
    /// ([`Incoming::Undecryptable`]). Doesn't borrow the backend.
    fn send_retry_request(
        &mut self,
        sender: ServiceId,
        retry: RetryRequest,
    ) -> LocalBoxFuture<'static, Result<(), BackendError>>;

    /// Sends `content` again to `recipient` alone, with its timestamp
    /// `sent_at` ([`Incoming::RetryRequested`]). Doesn't borrow the backend.
    fn resend(
        &mut self,
        recipient: ServiceId,
        content: Vec<u8>,
        sent_at: Timestamp,
    ) -> LocalBoxFuture<'static, Result<(), BackendError>>;

    /// Sends `recipient` an empty message, which starts a new session after
    /// [`Incoming::RetryRequested`] reset it. Doesn't borrow the backend.
    fn send_null_message(
        &mut self,
        recipient: ServiceId,
    ) -> LocalBoxFuture<'static, Result<(), BackendError>>;

    /// Tells this account's other devices we deleted these messages, by
    /// author (`None` for ours) and sent time, for ourselves only. Doesn't
    /// borrow the backend.
    fn sync_deleted_for_me(
        &mut self,
        destination: Destination,
        messages: Vec<(Option<ServiceId>, Timestamp)>,
    ) -> LocalBoxFuture<'static, Result<(), BackendError>>;

    /// A group's state as last fetched, with member names from contacts and
    /// cached profiles; `None` if the group is unknown. Doesn't use the
    /// network, nor borrow the backend.
    fn group(
        &mut self,
        key: GroupMasterKey,
    ) -> LocalBoxFuture<'static, Result<Option<GroupInfo>, BackendError>>;

    /// Fetches and caches the profiles of the group's members that have no
    /// name yet; [`Self::group`] then has their names. Doesn't borrow the
    /// backend.
    fn fetch_member_profiles(
        &mut self,
        key: GroupMasterKey,
    ) -> LocalBoxFuture<'static, Result<(), BackendError>>;

    /// Checks the open connection with a round trip to the server. The core
    /// times it out.
    fn probe(&mut self) -> LocalBoxFuture<'static, Result<(), BackendError>>;

    /// The conversation's disappearing messages timer in seconds, as
    /// [`Self::send`] applies it; `None` if off. Doesn't use the network,
    /// nor borrow the backend.
    fn expire_timer(
        &mut self,
        destination: Destination,
    ) -> LocalBoxFuture<'static, Result<Option<u32>, BackendError>>;

    /// `peer`'s identity, once a session with them exists. Doesn't use the
    /// network, nor borrow the backend.
    fn identity(
        &mut self,
        peer: ServiceId,
    ) -> LocalBoxFuture<'static, Result<Option<PeerIdentity>, BackendError>>;

    /// Accounts whose identity key changed, as sessions with them saw it,
    /// while receiving or sending; only the first call gets them.
    fn identity_changes(&mut self) -> LocalBoxStream<'static, ServiceId>;

    /// This account's ID, e.g. the peer of Note to Self.
    fn own_id(&self) -> ServiceId;

    /// This device's name, as the phone lists it.
    fn device_name(&self) -> Option<String>;

    /// Asks the phone to send its contacts; they arrive as
    /// [`Incoming::ContactsSynced`]. Doesn't borrow the backend.
    fn request_contact_sync(&mut self) -> LocalBoxFuture<'static, Result<(), BackendError>>;

    /// Asks the phone to send the account's settings; they arrive as
    /// [`Incoming::Settings`]. Doesn't borrow the backend.
    fn request_settings_sync(&mut self) -> LocalBoxFuture<'static, Result<(), BackendError>>;

    /// Tells `recipient` we read their messages sent at `timestamps`.
    /// Doesn't borrow the backend.
    fn send_read_receipts(
        &mut self,
        recipient: ServiceId,
        timestamps: Vec<Timestamp>,
    ) -> LocalBoxFuture<'static, Result<(), BackendError>>;

    /// Tells this account's other devices we read these messages, by author
    /// and sent time. Doesn't borrow the backend.
    fn send_read_sync(
        &mut self,
        read: Vec<(ServiceId, Timestamp)>,
    ) -> LocalBoxFuture<'static, Result<(), BackendError>>;

    /// Tells `destination` we started or stopped typing. Doesn't borrow
    /// the backend.
    fn send_typing(
        &mut self,
        destination: Destination,
        typing: bool,
    ) -> LocalBoxFuture<'static, Result<(), BackendError>>;

    /// Removes this device from the account, so the phone stops listing it.
    /// Doesn't borrow the backend.
    fn remove_device(&mut self) -> LocalBoxFuture<'static, Result<(), BackendError>>;
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
